// SPDX-License-Identifier: GPL-3.0-or-later
//! Explicit local developer/session control. No synthetic XR pose is advertised.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use xr_shell_protocols::Client;
fn main() -> Result<()> {
    let mut socket = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/tmp".into())
        .join("xr-desktop/shell.sock");
    let mut kind = "GetSnapshot".to_owned();
    let mut payload = json!({});
    let mut args = std::env::args().skip(1);
    let mut click = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = args.next().context("socket missing")?.into(),
            "--request" => kind = args.next().context("request missing")?,
            "--payload" => {
                payload = serde_json::from_str(&args.next().context("payload missing")?)?
            }
            "--click" => {
                let component = args.next().context("component id missing")?;
                let node = args.next().context("node id missing")?;
                click = Some((component, node));
            }
            "--help" => {
                println!(
                    "xr-shellctl [--socket PATH] [--request TYPE --payload JSON] [--click COMPONENT_ID NODE_ID]\nDebugInput requires Shell --debug-input. Requests carry real component generations and are routed by Shell."
                );
                return Ok(());
            }
            _ => bail!("unknown option {arg}"),
        }
    }
    let required =
        if ["DebugInput", "GetScene", "GetLayout"].contains(&kind.as_str()) || click.is_some() {
            vec!["core.v1", "debug.v1"]
        } else {
            vec!["core.v1", "snapshot.v1"]
        };
    let mut client = Client::connect(&socket, "system_ui", &required)?;
    if let Some((component, node)) = click {
        for phase in ["press", "release"] {
            let id = client.send(
                "DebugInput",
                json!({"kind":"pointer","component_id":component,"node_id":node,"phase":phase}),
                &[],
            )?;
            wait(&client, &id)?;
        }
        return Ok(());
    }
    let id = client.send(&kind, payload, &[])?;
    let result = wait(&client, &id)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
fn wait(client: &Client, id: &str) -> Result<Value> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if let Some(packet) = client.recv(200)? {
            if packet.message["request_id"] == id {
                if packet.message["type"] == "Error" || packet.message["type"] == "SceneRejected" {
                    bail!("{}", packet.message);
                }
                return Ok(packet.message);
            }
        }
    }
    bail!("Shell reply timeout")
}
