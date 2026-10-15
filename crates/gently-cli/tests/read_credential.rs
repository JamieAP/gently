use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

#[test]
fn queries_use_the_read_credential_without_requiring_or_sending_ingest_access() {
    for include_ingest in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            connection
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(connection.try_clone().unwrap());
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                assert!(!line.is_empty());
                headers.push_str(&line);
            }
            assert!(headers
                .to_lowercase()
                .contains("authorization: bearer synthetic-reader-token"));
            assert!(!headers.contains("synthetic-ingest-token"));
            connection.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]").unwrap();
        });
        let mut command = assert_cmd::Command::cargo_bin("gently").unwrap();
        command
            .env_clear()
            .env("HOME", dir.path())
            .env("GENTLY_STATE_DIR", dir.path())
            .env("PATH", "/usr/bin:/bin")
            .env("GENTLY_COLLECTOR_URL", url)
            .env("GENTLY_READ_TOKEN", "synthetic-reader-token")
            .args(["traces", "--json"]);
        if include_ingest {
            command.env("GENTLY_TOKEN", "synthetic-ingest-token");
        }
        command.assert().success().stdout("[]\n");
        server.join().unwrap();
    }
}
