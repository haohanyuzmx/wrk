use bytes::Bytes;
use http_body_util::Empty;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use wrk::transport_layer::processor::Http1Handle;
use wrk::transport_layer::transport::{TcpSteamMaker, TimeOutStop};
use wrk::transport_layer::Pressure;

fn main() {
    // env::set_var("RUST_BACKTRACE", "1");
    //
    tracing_subscriber::fmt()
        .with_line_number(true)
        .with_max_level(tracing::Level::INFO)
        .init();
    // info!("test out");
    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap(),
    );

    //let a = Arc::new(AtomicBool::new(false));
    let a = TimeOutStop::new(Duration::from_secs(5));
    let cpus = num_cpus::get();
    let mut ht = None;
    for _ in 0..cpus - 2 {
        let n_rt = rt.clone();
        let a_c = a.clone();
        ht = Some(thread::spawn(|| {
            let transport_conn = TcpSteamMaker::new("127.0.0.1:8080");
            let processor =
                Http1Handle::new("http://127.0.0.1:8080", Empty::<Bytes>::new()).unwrap();
            let pressure = Pressure::new(n_rt, transport_conn, processor, None);
            pressure.run(a_c);
        }));
    }
    let _ = ht.unwrap().join();
}
