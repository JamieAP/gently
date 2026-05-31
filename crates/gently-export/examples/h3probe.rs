//! POST a minimal OTLP body to /v1/traces over forced HTTP/3 and print the
//! protocol the edge echoes back - proves the ingest path runs over QUIC.
#[tokio::main]
async fn main() {
    let url = std::env::args().nth(1).expect("usage: h3probe <collector_url>");
    let token = std::env::var("GENTLY_TOKEN").expect("GENTLY_TOKEN");
    let client = reqwest::Client::builder().http3_prior_knowledge().build().expect("h3 client");
    let resp = client
        .post(format!("{}/v1/traces", url.trim_end_matches('/')))
        .version(reqwest::Version::HTTP_3)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(r#"{"resourceSpans":[]}"#)
        .send().await.expect("request");
    println!("ingest: client={:?} status={} body={}", resp.version(), resp.status(), resp.text().await.unwrap_or_default());
}
