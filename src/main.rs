use bytes::Bytes;
use http_body_util::Empty;
use std::io::stdout;
use std::sync::Arc;
use std::thread;

use wrk::transport_layer::processor::Http1Handle;
use wrk::transport_layer::transport::TcpSteamMaker;
use wrk::transport_layer::Pressure;

fn main() {
    // env::set_var("RUST_BACKTRACE", "1");
    //
    // tracing_subscriber::fmt()
    //     .with_line_number(true)
    //     .with_max_level(tracing::Level::DEBUG)
    //     .init();
    // info!("test out");
    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap(),
    );

    let cpus = num_cpus::get();
    let mut ht = None;
    for _ in 0..cpus - 1 {
        let n_rt = rt.clone();
        ht = Some(thread::spawn(|| {
            let transport_conn = TcpSteamMaker::new("127.0.0.1:8080");
            let processor =
                Http1Handle::new("http://127.0.0.1:8080", Empty::<Bytes>::new()).unwrap();
            let pressure = Pressure::new(n_rt, transport_conn, processor, None);
            pressure.run(Box::new(stdout()));
        }));
    }
    let _ = ht.unwrap().join();
}
