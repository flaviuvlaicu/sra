use crossterm::{
    event::{poll, read, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, size as terminal_size},
};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

const MSG_DATA: u8 = 0x00;
const MSG_RESIZE: u8 = 0x01;
const SHELL_PORT: u16 = 22222;

/// Run an interactive shell session on the named agent.
///
/// passphrase: if Some, passes -k to `sra connect` for XChaCha20-Poly1305 E2E encryption.
/// Also checks SRA_E2EE_KEY env var as fallback (avoids shell history exposure).
pub fn run_shell(agent: &str, passphrase: Option<&str>) -> anyhow::Result<()> {
    let env_key = std::env::var("SRA_E2EE_KEY").ok();
    let effective_key = passphrase.or(env_key.as_deref());

    let mut args = vec!["connect".to_string(), "-n".to_string(), agent.to_string()];
    if let Some(key) = effective_key {
        args.push("-k".to_string());
        args.push(key.to_string());
    }
    args.push("127.0.0.1".to_string());
    args.push(SHELL_PORT.to_string());

    let mut child = Command::new(std::env::current_exe()?)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())  // logs go to stderr, not stdout
        .spawn()?;

    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout_pipe = child.stdout.take().expect("stdout");

    // Send initial terminal size: cols (u16 BE) then rows (u16 BE)
    let (cols, rows) = terminal_size()?;
    stdin.write_all(&cols.to_be_bytes())?;
    stdin.write_all(&rows.to_be_bytes())?;
    stdin.flush()?;

    let enc_label = if effective_key.is_some() { " [E2E encrypted]" } else { "" };
    eprintln!("\x1b[32m[sra] connected to {}{} — Ctrl+] to exit\x1b[0m", agent, enc_label);

    enable_raw_mode()?;

    // Thread: agent PTY output → local stdout
    let output_thread = std::thread::spawn(move || {
        let mut header = [0u8; 1];
        let mut out = std::io::stdout();
        loop {
            if stdout_pipe.read_exact(&mut header).is_err() { break; }
            if header[0] == MSG_DATA {
                let mut len_buf = [0u8; 2];
                if stdout_pipe.read_exact(&mut len_buf).is_err() { break; }
                let len = u16::from_be_bytes(len_buf) as usize;
                let mut data = vec![0u8; len];
                if stdout_pipe.read_exact(&mut data).is_err() { break; }
                if out.write_all(&data).is_err() { break; }
                let _ = out.flush();
            }
        }
    });

    // Main thread: keyboard + resize events → agent PTY
    loop {
        if poll(Duration::from_millis(50))? {
            match read()? {
                Event::Key(key) => {
                    // Ctrl+] exits (telnet-style escape)
                    if key.code == KeyCode::Char(']')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }
                    let bytes = key_to_bytes(key.code, key.modifiers);
                    if bytes.is_empty() { continue; }
                    let len = bytes.len() as u16;
                    let mut frame = Vec::with_capacity(3 + bytes.len());
                    frame.push(MSG_DATA);
                    frame.extend_from_slice(&len.to_be_bytes());
                    frame.extend_from_slice(&bytes);
                    stdin.write_all(&frame)?;
                    stdin.flush()?;
                }
                Event::Resize(cols, rows) => {
                    let mut frame = vec![MSG_RESIZE];
                    frame.extend_from_slice(&cols.to_be_bytes());
                    frame.extend_from_slice(&rows.to_be_bytes());
                    stdin.write_all(&frame)?;
                    stdin.flush()?;
                }
                _ => {}
            }
        }
        if output_thread.is_finished() { break; }
    }

    disable_raw_mode()?;
    println!();
    let _ = child.kill();
    let _ = child.wait();
    eprintln!("\x1b[32m[sra] session ended\x1b[0m");
    Ok(())
}

fn key_to_bytes(code: KeyCode, modifiers: KeyModifiers) -> Vec<u8> {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Char(c) => {
            if ctrl {
                let b = c as u8;
                if b.is_ascii_lowercase() { vec![b - b'a' + 1] }
                else if b.is_ascii_uppercase() { vec![b - b'A' + 1] }
                else { c.to_string().into_bytes() }
            } else { c.to_string().into_bytes() }
        }
        KeyCode::Enter     => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Delete    => vec![0x1b, b'[', b'3', b'~'],
        KeyCode::Tab       => vec![b'\t'],
        KeyCode::Esc       => vec![0x1b],
        KeyCode::Up        => vec![0x1b, b'[', b'A'],
        KeyCode::Down      => vec![0x1b, b'[', b'B'],
        KeyCode::Right     => vec![0x1b, b'[', b'C'],
        KeyCode::Left      => vec![0x1b, b'[', b'D'],
        KeyCode::Home      => vec![0x1b, b'[', b'H'],
        KeyCode::End       => vec![0x1b, b'[', b'F'],
        KeyCode::PageUp    => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown  => vec![0x1b, b'[', b'6', b'~'],
        KeyCode::F(1)      => vec![0x1b, b'O', b'P'],
        KeyCode::F(2)      => vec![0x1b, b'O', b'Q'],
        KeyCode::F(3)      => vec![0x1b, b'O', b'R'],
        KeyCode::F(4)      => vec![0x1b, b'O', b'S'],
        _                  => vec![],
    }
}
