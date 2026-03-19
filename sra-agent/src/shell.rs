use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{error, info};

pub const SHELL_PORT: u16 = 22222;

const MSG_DATA: u8 = 0x00;
const MSG_RESIZE: u8 = 0x01;

enum PtyMsg {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Close,
}

pub async fn start_shell_server() {
    let listener = match TcpListener::bind(format!("127.0.0.1:{}", SHELL_PORT)).await {
        Ok(l) => l,
        Err(e) => {
            error!("SRA shell server failed to bind on port {}: {}", SHELL_PORT, e);
            return;
        }
    };
    info!("SRA shell server listening on 127.0.0.1:{}", SHELL_PORT);
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                info!("New shell session from {}", peer);
                tokio::spawn(async move {
                    if let Err(e) = handle_session(stream).await {
                        error!("Shell session error: {}", e);
                    }
                });
            }
            Err(e) => error!("Shell server accept error: {}", e),
        }
    }
}

async fn handle_session(mut stream: TcpStream) -> anyhow::Result<()> {
    // Client sends initial terminal size: cols (u16 BE) then rows (u16 BE)
    let cols = stream.read_u16().await.unwrap_or(220);
    let rows = stream.read_u16().await.unwrap_or(50);

    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows, cols, pixel_width: 0, pixel_height: 0,
    })?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| {
        if std::path::Path::new("/bin/bash").exists() {
            "/bin/bash".to_string()
        } else {
            "/bin/sh".to_string()
        }
    });

    let mut cmd = CommandBuilder::new(&shell);
    cmd.env("TERM", "xterm-256color");

    let _child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    // Use channels to avoid Arc/Clone issues with PtyMaster
    let (pty_tx, mut pty_rx) = mpsc::channel::<PtyMsg>(64);
    let pty_tx_close = pty_tx.clone();

    let mut master_reader = pair.master.try_clone_reader()?;
    let mut master_writer = pair.master.take_writer()?;
    let master = pair.master;

    let (mut tcp_read, mut tcp_write) = stream.into_split();

    // Blocking thread: owns master for resize, handles input writes
    // Separate sub-thread for PTY reads
    let pty_thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Handle::current();

        // Sub-thread: reads PTY output and sends to TCP
        let reader_thread = std::thread::spawn(move || {
            let rt2 = tokio::runtime::Handle::current();
            let mut buf = [0u8; 4096];
            loop {
                match master_reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let len = n as u16;
                        let mut frame = Vec::with_capacity(3 + n);
                        frame.push(MSG_DATA);
                        frame.extend_from_slice(&len.to_be_bytes());
                        frame.extend_from_slice(&buf[..n]);
                        if rt2.block_on(tcp_write.write_all(&frame)).is_err() { break; }
                        let _ = rt2.block_on(tcp_write.flush());
                    }
                }
            }
        });

        // Process input and resize messages from async side
        while let Some(msg) = rt.block_on(pty_rx.recv()) {
            match msg {
                PtyMsg::Input(data) => {
                    if master_writer.write_all(&data).is_err() { break; }
                }
                PtyMsg::Resize { cols, rows } => {
                    let _ = master.resize(PtySize {
                        rows, cols, pixel_width: 0, pixel_height: 0,
                    });
                }
                PtyMsg::Close => break,
            }
        }
        let _ = reader_thread.join();
    });

    // Async task: reads TCP and routes to PTY channel
    let tcp_to_pty = tokio::spawn(async move {
        let mut msg_type = [0u8; 1];
        loop {
            if tcp_read.read_exact(&mut msg_type).await.is_err() { break; }
            match msg_type[0] {
                MSG_DATA => {
                    let len = match tcp_read.read_u16().await {
                        Ok(n) => n as usize, Err(_) => break,
                    };
                    let mut data = vec![0u8; len];
                    if tcp_read.read_exact(&mut data).await.is_err() { break; }
                    if pty_tx.send(PtyMsg::Input(data)).await.is_err() { break; }
                }
                MSG_RESIZE => {
                    let cols = match tcp_read.read_u16().await { Ok(n) => n, Err(_) => break };
                    let rows = match tcp_read.read_u16().await { Ok(n) => n, Err(_) => break };
                    if pty_tx.send(PtyMsg::Resize { cols, rows }).await.is_err() { break; }
                }
                _ => break,
            }
        }
        let _ = pty_tx.send(PtyMsg::Close).await;
    });

    tcp_to_pty.await?;
    let _ = pty_tx_close.send(PtyMsg::Close).await;
    let _ = tokio::task::spawn_blocking(move || pty_thread.join()).await;

    info!("Shell session ended");
    Ok(())
}
