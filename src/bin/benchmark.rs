use aiub_notice_checker::scraper::parse_listing_html;
use serde::Serialize;
use std::env;
use std::fs;
use std::process;

#[derive(Debug, Serialize)]
struct BenchmarkNotice {
    pub index: usize,
    pub title: String,
    pub body: String,
    pub link: String,
    pub published_date: String,
}

#[derive(Debug, Serialize)]
struct BenchmarkReport {
    pub version: String,
    pub timestamp: String,
    pub total_notices_scraped: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub total_duration_ms: u64,
    pub avg_latency_per_notice_ms: u64,
    pub html_size_bytes: usize,
    pub threshold_met: bool,
    pub notices: Vec<BenchmarkNotice>,
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "Usage: {} <html_path> <version> <duration_ms> <output_json_path>",
            args.first().map(String::as_str).unwrap_or("benchmark")
        );
        process::exit(1);
    }

    let html_path = &args[1];
    let version = &args[2];
    let duration_ms: u64 = args[3].parse().unwrap_or(0);
    let output_json_path = &args[4];

    let html = match fs::read_to_string(html_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to read HTML file at {}: {}", html_path, e);
            process::exit(1);
        }
    };

    let html_size = html.len();
    let raw_notices = parse_listing_html(&html);
    let total_scraped = raw_notices.len();

    let mut notices = Vec::with_capacity(total_scraped);
    for (i, notice) in raw_notices.into_iter().enumerate() {
        let body = if !notice.body_text.is_empty() {
            notice.body_text
        } else if !notice.description.is_empty() {
            notice.description
        } else {
            "No description available".to_string()
        };

        notices.push(BenchmarkNotice {
            index: i + 1,
            title: notice.title,
            body,
            link: notice.url,
            published_date: notice
                .published_date
                .unwrap_or_else(|| "Unknown".to_string()),
        });
    }

    let avg_latency_ms = if total_scraped > 0 {
        duration_ms / (total_scraped as u64)
    } else {
        0
    };

    let threshold_met = total_scraped >= 500;
    let failure_count = if threshold_met { 0 } else { 1 };

    let timestamp = chrono::Utc::now().to_rfc3339();

    let report = BenchmarkReport {
        version: version.clone(),
        timestamp,
        total_notices_scraped: total_scraped,
        success_count: total_scraped,
        failure_count,
        total_duration_ms: duration_ms,
        avg_latency_per_notice_ms: avg_latency_ms,
        html_size_bytes: html_size,
        threshold_met,
        notices,
    };

    let json = match serde_json::to_string_pretty(&report) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Failed to serialize benchmark report: {}", e);
            process::exit(1);
        }
    };

    if let Err(e) = fs::write(output_json_path, json) {
        eprintln!("Failed to write output JSON to {}: {}", output_json_path, e);
        process::exit(1);
    }

    println!(
        "Successfully scraped and benchmarked {} notices (threshold_met={})",
        total_scraped, threshold_met
    );

    if !threshold_met {
        eprintln!(
            "ERROR: Scraper benchmark failed — scraped {} notices, expected >= 500",
            total_scraped
        );
        process::exit(1);
    }
}
