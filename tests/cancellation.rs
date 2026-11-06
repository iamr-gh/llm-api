use llm_api::{Cancelled, Client, Provider, Request};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

fn request() -> Request {
    serde_json::from_value(json!({"model":"test", "max_tokens":100,
        "messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}]}))
    .unwrap()
}

fn read_request(socket: &mut TcpStream) {
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(socket);
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    reader.read_exact(&mut vec![0; length]).unwrap();
}

fn client(provider: Provider, endpoint: String) -> Client {
    let client = if provider == Provider::Codex {
        Client::codex("dummy", "account").unwrap()
    } else {
        Client::new(provider, "dummy")
    };
    client.with_endpoint(endpoint).unwrap()
}

#[test]
fn pre_cancelled_requests_never_connect() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    server.set_nonblocking(true).unwrap();
    let client = client(
        Provider::Openai,
        format!("http://{}", server.local_addr().unwrap()),
    );
    let error = client
        .run_cancellable(&request(), &AtomicBool::new(true), |_| {
            panic!("event after cancellation")
        })
        .unwrap_err();
    assert!(error.is::<Cancelled>());
    assert_eq!(
        server.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn cancellation_closes_idle_headers_body_partial_sse_and_tls() {
    for provider in [
        Provider::Openai,
        Provider::Anthropic,
        Provider::Openrouter,
        Provider::Codex,
    ] {
        for stage in ["headers", "body", "partial", "tls"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!(
                "{}://{}",
                if stage == "tls" { "https" } else { "http" },
                listener.local_addr().unwrap()
            );
            let (ready, wait) = mpsc::channel();
            let server = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                if stage == "tls" {
                    assert!(socket.read(&mut [0; 4096]).unwrap() > 0);
                } else {
                    read_request(&mut socket);
                    if stage != "headers" {
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").unwrap();
                        if stage == "partial" {
                            socket.write_all(b"data: {\"type\":").unwrap();
                        }
                    }
                }
                ready.send(()).unwrap();
                let mut buffer = [0; 4096];
                loop {
                    match socket.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(_) => continue,
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::ConnectionAborted
                            ) =>
                        {
                            break;
                        }
                        Err(e) => panic!("socket not closed on {stage}: {e}"),
                    }
                }
            });
            let cancelled = AtomicBool::new(false);
            thread::scope(|scope| {
                let cancel = &cancelled;
                scope.spawn(move || {
                    wait.recv_timeout(Duration::from_secs(3)).unwrap();
                    thread::sleep(Duration::from_millis(30));
                    cancel.store(true, Ordering::Relaxed);
                });
                let start = Instant::now();
                let result =
                    client(provider, endpoint)
                        .run_cancellable(&request(), &cancelled, |_| panic!("unexpected event"));
                assert!(
                    result.unwrap_err().is::<Cancelled>(),
                    "{provider:?}/{stage}"
                );
                assert!(
                    start.elapsed() < Duration::from_secs(2),
                    "{provider:?}/{stage}"
                );
            });
            server.join().unwrap();
        }
    }
}

fn completed() -> String {
    format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":{"output":[
        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],"usage":{}}})
    )
}

#[test]
fn cancelling_one_call_does_not_cancel_another_or_poison_client() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (ready, wait) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stalled, _) = listener.accept().unwrap();
        read_request(&mut stalled);
        ready.send(()).unwrap();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            read_request(&mut socket);
            let body = completed();
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        assert_eq!(stalled.read(&mut [0]).unwrap(), 0);
    });
    let client = client(Provider::Openai, endpoint);
    let cancelled = AtomicBool::new(false);
    thread::scope(|scope| {
        let worker = scope.spawn(|| client.run_cancellable(&request(), &cancelled, |_| true));
        wait.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            client
                .run_cancellable(&request(), &AtomicBool::new(false), |_| true)
                .unwrap()
                .finish_reason,
            "stop"
        );
        cancelled.store(true, Ordering::Relaxed);
        assert!(worker.join().unwrap().unwrap_err().is::<Cancelled>());
        assert_eq!(
            client.run(&request(), |_| true).unwrap().finish_reason,
            "stop"
        );
    });
    server.join().unwrap();
}

#[test]
fn cancellation_after_a_delta_closes_socket_without_waiting_for_another_event() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        read_request(&mut socket);
        let delta = json!({"type":"response.output_text.delta","output_index":0,"delta":"partial"});
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\ndata: {delta}\n\n"
        )
        .unwrap();
        assert_eq!(socket.read(&mut [0]).unwrap(), 0);
    });
    let cancelled = AtomicBool::new(false);
    let mut events = 0;
    let result =
        client(Provider::Openai, endpoint).run_cancellable(&request(), &cancelled, |event| {
            assert_eq!(event["delta"], "partial");
            events += 1;
            cancelled.store(true, Ordering::Relaxed);
            true
        });
    assert_eq!(events, 1);
    assert!(result.unwrap_err().is::<Cancelled>());
    server.join().unwrap();
}

#[test]
fn callback_panic_also_joins_the_cancellation_monitor() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        read_request(&mut socket);
        let delta = json!({"type":"response.output_text.delta","output_index":0,"delta":"partial"});
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\ndata: {delta}\n\n"
        )
        .unwrap();
        assert_eq!(socket.read(&mut [0]).unwrap(), 0);
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client(Provider::Openai, endpoint).run_cancellable(
            &request(),
            &AtomicBool::new(false),
            |_| panic!("caller panic"),
        )
    }));
    assert!(result.is_err());
    server.join().unwrap();
}

#[test]
fn callback_cancellation_is_typed_and_does_not_require_send() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        read_request(&mut socket);
        let body = format!(
            "data: {}\n\n",
            json!({"type":"response.output_text.delta","output_index":0,"delta":"hi"})
        );
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let seen = std::rc::Rc::new(std::cell::Cell::new(false));
    let result = client(Provider::Openai, endpoint).run_cancellable(
        &request(),
        &AtomicBool::new(false),
        |_| {
            seen.set(true);
            false
        },
    );
    assert!(seen.get());
    assert!(result.unwrap_err().is::<Cancelled>());
    server.join().unwrap();
}
