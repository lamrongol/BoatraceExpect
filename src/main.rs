use chrono::{DateTime, Datelike, NaiveDate, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use reqwest::header::{HeaderMap, USER_AGENT};
use scraper::{Html, Selector};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::fs::create_dir;
use std::process::exit;
use std::time::Duration;
use std::{env, fs, thread};

const BASE_URL: &str = "https://www.boatrace.jp/owpc/pc/race/";
const TIMEZONE: Tz = Tz::Asia__Tokyo;

#[tokio::main]
async fn main() {
    let mut rsrc_file = std::env::current_exe().expect("Can't find path to executable");
    rsrc_file.pop();
    rsrc_file.pop();
    rsrc_file.pop();
    let data_dir = rsrc_file.join("docs").join("v3");

    let mut args: Vec<String> = env::args().collect();
    if args.last().unwrap() == "--release" {
        args.pop();
    }
    //GitHub Workflow上での調査のため
    println!("Args: {:?}", args);
    let (date, json_str) = if args.len() == 1 {
        //no argument
        let today = Utc::now().with_timezone(&TIMEZONE).naive_local().date();
        (today, scraping(&today).await)
    } else if args.len() == 2 {
        let record_file = data_dir.join("recorded_day.txt");
        let date = fs::read_to_string(&record_file).unwrap();
        let date = NaiveDate::parse_from_str(&date, "%Y-%m-%d").unwrap() - TimeDelta::days(1);
        let tmp = scraping(&date).await;
        fs::write(&record_file, date.to_string()).unwrap();
        (date, tmp)
    } else {
        let date = NaiveDate::parse_from_str(&args[2], "%Y%m%d").unwrap();
        let tmp = scraping(&date).await;
        (date, tmp)
    };

    let dir = data_dir.join(date.year().to_string());
    if !dir.exists() {
        create_dir(dir.clone()).unwrap();
    }
    let file_name = dir.join(format!(
        "{}{:02}{:02}.json",
        date.year(),
        date.month(),
        date.day()
    ));
    fs::write(&file_name, json_str).unwrap();
}
const RETRY_CNT: usize = 3;
async fn scraping(date: &NaiveDate) -> String {
    let json_date = format!("{}-{:02}-{:02}", date.year(), date.month(), date.day());
    let date = format!("{}{:02}{:02}", date.year(), date.month(), date.day());
    let date_url = format!("{BASE_URL}index?hd={}", &date);
    let html = fetch(&date_url).await.unwrap();
    let document = Html::parse_document(&html);
    let mut stadium_no_list = vec![];
    for a in document
        .select(&Selector::parse("div.table1 table tbody tr:first-child td.is-p10-7 > a").unwrap())
    {
        let url = a.attr("href").unwrap();
        let no = url
            .split_once("?")
            .unwrap()
            .1
            .split_once("&")
            .unwrap()
            .0
            .split_once("=")
            .unwrap()
            .1;
        stadium_no_list.push(no);
    }

    let mut expect_list = vec![];
    for stadium_no in stadium_no_list {
        let url = format!("{BASE_URL}raceindex?jcd={stadium_no}&hd={date}");
        let html = fetch(&url).await.unwrap();
        let document = Html::parse_document(&html);
        let mut race_no_list = vec![];
        for tr in document.select(&Selector::parse("div.table1 table tbody tr").unwrap()) {
            let a = tr
                .select(&Selector::parse("td:nth-child(1) a").unwrap())
                .next()
                .unwrap();
            let url = a.attr("href").unwrap();
            let no = url
                .split_once("?")
                .unwrap()
                .1
                .split_once("&")
                .unwrap()
                .0
                .split_once("=")
                .unwrap()
                .1;
            race_no_list.push(no);
        }
        for race_no in race_no_list {
            dbg!(stadium_no, race_no);
            //正常に取得できたかチェックしてダメなら再取得(三回まで)
            for i in 0..RETRY_CNT {
                let url = format!("{BASE_URL}pcexpect?rno={race_no}&jcd={stadium_no}&hd={date}");
                let html = fetch(&url).await.unwrap();

                let document = Html::parse_document(&html);
                let tmp = document
                    .select(&Selector::parse("p.state2_lv").unwrap())
                    .next();
                //取得チェック
                if tmp.is_none() {
                    println!("データが取得できませんでした");
                    if i == RETRY_CNT - 1 {
                        exit(1);
                    }
                    thread::sleep(Duration::from_mins(1));
                    continue;
                }
                let confidence = tmp.unwrap();

                let confidence_level = confidence
                    .attr("class")
                    .unwrap()
                    .chars()
                    .last()
                    .unwrap()
                    .to_string()
                    .parse::<i64>()
                    .unwrap();
                let mut map = BTreeMap::new();
                for tr in document.select(
                    &Selector::parse(
                        "div.contentsFrame1_inner div:nth-child(6) table tbody tr:nth-child(1)",
                    )
                    .unwrap(),
                ) {
                    let boat_td = tr
                        .select(&Selector::parse("td:nth-child(2)").unwrap())
                        .next()
                        .unwrap();
                    let boat_number = boat_td.text().next().unwrap().parse::<u8>().unwrap();

                    let expect_img = tr
                        .select(&Selector::parse("td:nth-child(1) img").unwrap())
                        .next();
                    let expect_level = if expect_img.is_some() {
                        let chars = expect_img
                            .unwrap()
                            .attr("src")
                            .unwrap()
                            .chars()
                            .collect::<Vec<_>>();
                        let tmp = chars[chars.len() - 5].to_string().parse::<i64>().unwrap();
                        //△と☓の数値が逆になってる？
                        if tmp == 3 {
                            4
                        } else if tmp == 4 {
                            3
                        } else {
                            tmp
                        }
                    } else {
                        5
                    };
                    map.insert(boat_number, expect_level);
                }
                expect_list.push(Expect {
                    date: json_date.clone(),
                    stadium_number: stadium_no.parse().unwrap(),
                    number: race_no.parse().unwrap(),
                    confidence_level,
                    expect_level: map,
                });

                //エラーが起きなかったらbreak
                break;
            }
        }
    }

    serde_json::to_string(&Wrapper {
        expect: expect_list,
    })
    .unwrap()
}

#[derive(Debug, Serialize)]
struct Expect {
    date: String,
    stadium_number: i64,
    number: i64,
    confidence_level: i64,
    expect_level: BTreeMap<u8, i64>,
}

#[derive(Debug, Serialize)]
struct Wrapper {
    expect: Vec<Expect>,
}

pub async fn fetch(url: &str) -> Result<String, reqwest::Error> {
    thread::sleep(Duration::from_millis(2000 + rand::random_range(0..1000)));
    // Some sites block requests with no/odd User-Agent.
    // This is a simple, polite one.
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, "rust-web-scraper/0.1".parse().unwrap());
    // Client is built once per call here for simplicity.
    // If you're scraping multiple URLs, build it once and reuse it (see note below).
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()?;

    client
        .get(url)
        .send()
        .await?
        // Turns 404/500 into an error right here.
        // Without this, you'd happily parse a "Not Found" HTML page.
        .error_for_status()?
        .text()
        .await
}
