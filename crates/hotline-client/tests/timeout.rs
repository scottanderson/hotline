//! Requests against a server that never replies.

use hotline_client::{ClientError, HotlineClient};
use hotline_proto::{ClientHandshake, ServerHandshake};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test(start_paused = true)]
async fn request_times_out_when_the_server_never_replies() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut handshake = [0; ClientHandshake::LEN];
        stream.read_exact(&mut handshake).await.unwrap();
        stream
            .write_all(&ServerHandshake::OK.encode())
            .await
            .unwrap();
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink).await;
    });

    let (client, _events) = HotlineClient::connect(addr).await.unwrap();
    let result = client.get_user_name_list().await;

    assert!(matches!(result, Err(ClientError::Timeout)));
    drop(client);
    server.abort();
}
