//! The Jade's pin server is reached with minreq over HTTPS. The app has both rustls crypto
//! providers compiled in (ring for Electrum, aws-lc-rs through reqwest), and rustls panics when
//! asked to pick one itself, which killed the signing thread and left the prompt on "checking".
//! Creating the hub, which every run of the app does first, chooses one.
use std::net::TcpListener;

#[test]
fn an_https_request_from_a_device_job_fails_cleanly_instead_of_panicking() {
    let (_hub, _inbox) = coldbench::work::hub(&egui::Context::default(), None);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            drop(s);
        }
    });
    let r = minreq::get(format!("https://127.0.0.1:{port}/")).with_timeout(5).send();
    assert!(r.is_err(), "nothing answers TLS there, so it is an error and not a panic");
}
