use aiub_notice_checker::scraper::parse_listing_html;
use serde::Serialize;
use std::env;
use std::fs;
use std::process;
use std::time::Instant;

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
    /// O2: the fetch duration passed in by the release workflow (curl
    /// wall time). This is NOT parse latency — the old report divided
    /// this by the notice count and called it latency, which is why
    /// 6,263 ms vs 3,859 ms looked like a regression on identical HTML.
    pub total_duration_ms: u64,
    /// O2: parse-only timing — `parse_listing_html` run
    /// `PARSE_RUNS` times over the already-fetched HTML, with the
    /// median reported. This is the actual parser performance signal;
    /// the fetch duration above is network-bound and must not be used
    /// as one.
    pub parse_runs: usize,
    pub parse_durations_ms: Vec<u64>,
    pub parse_duration_ms_median: u64,
    pub html_size_bytes: usize,
    pub threshold_met: bool,
    pub notices: Vec<BenchmarkNotice>,
}

/// O2: number of parse repetitions; the median discards a cold first
/// run (allocator warm-up) and any single slow outlier.
pub const PARSE_RUNS: usize = 5;

/// Median of a slice (upper median for even lengths). An empty slice
/// returns 0 — the benchmark always passes `PARSE_RUNS` durations, so
/// this is only a guard against an index panic, not a real signal.
/// Pure and unit-tested below.
pub fn median_u64(values: &mut [u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[values.len() / 2]
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
    // O2: time the parse itself, separately from the fetch. The fetch
    // duration arrives as an argument from the release workflow's curl
    // step; the parse runs here, PARSE_RUNS times, and the median is
    // the reported parse signal.
    let mut parse_durations_ms: Vec<u64> = Vec::with_capacity(PARSE_RUNS);
    let mut raw_notices = Vec::new();
    for run_idx in 0..PARSE_RUNS {
        let started = Instant::now();
        let parsed = parse_listing_html(&html);
        parse_durations_ms.push(started.elapsed().as_millis() as u64);
        if run_idx == 0 {
            raw_notices = parsed;
        }
    }
    let parse_duration_ms_median = median_u64(&mut parse_durations_ms.clone());
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
        parse_runs: PARSE_RUNS,
        parse_durations_ms,
        parse_duration_ms_median,
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
    println!(
        "Fetch: {} ms; parse (median of {} runs): {} ms",
        duration_ms, PARSE_RUNS, parse_duration_ms_median
    );

    if !threshold_met {
        eprintln!(
            "ERROR: Scraper benchmark failed — scraped {} notices, expected >= 500",
            total_scraped
        );
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::median_u64;

    #[test]
    fn median_odd_count() {
        assert_eq!(median_u64(&mut [5, 1, 3]), 3);
    }

    #[test]
    fn median_even_count_takes_upper() {
        assert_eq!(median_u64(&mut [4, 1, 3, 2]), 3);
    }

    #[test]
    fn median_single_value() {
        assert_eq!(median_u64(&mut [42]), 42);
    }

    #[test]
    fn median_ignores_a_slow_outlier() {
        // O2: a cold first run must not move the reported parse time.
        assert_eq!(median_u64(&mut [900, 12, 11, 13, 12]), 12);
    }

    #[test]
    fn median_empty_slice_returns_zero() {
        // Audit #20: the old indexing panicked on an empty slice.
        assert_eq!(median_u64(&mut []), 0);
    }
}
