//! i3 / Sway IPC (they speak the same protocol): window tree, workspaces and
//! commands over `$SWAYSOCK` / `$I3SOCK`.
//!
//! A message is `"i3-ipc"`, a native-endian u32 payload length, a native-endian
//! u32 message type, then the payload. Replies use the same framing.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

const MAGIC: &[u8; 6] = b"i3-ipc";
const RUN_COMMAND: u32 = 0;
const GET_TREE: u32 = 4;

/// the IPC socket of the running Sway or i3, Sway first
pub fn socket_path() -> Option<PathBuf> {
    ["SWAYSOCK", "I3SOCK"]
        .into_iter()
        .find_map(|name| std::env::var_os(name))
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

pub fn is_sway() -> bool {
    std::env::var_os("SWAYSOCK").is_some()
}

fn encode(kind: u32, payload: &str) -> Vec<u8> {
    let mut message = Vec::with_capacity(14 + payload.len());
    message.extend_from_slice(MAGIC);
    message.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    message.extend_from_slice(&kind.to_ne_bytes());
    message.extend_from_slice(payload.as_bytes());
    message
}

/// (payload length, message type) from a 14 byte reply header
fn decode_header(header: &[u8; 14]) -> Option<(usize, u32)> {
    if &header[..6] != MAGIC {
        return None;
    }
    let length = u32::from_ne_bytes(header[6..10].try_into().ok()?) as usize;
    let kind = u32::from_ne_bytes(header[10..14].try_into().ok()?);
    Some((length, kind))
}

fn request(kind: u32, payload: &str) -> Option<Value> {
    let mut stream = UnixStream::connect(socket_path()?).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(800))).ok()?;
    stream.set_write_timeout(Some(Duration::from_millis(800))).ok()?;
    stream.write_all(&encode(kind, payload)).ok()?;
    let mut header = [0u8; 14];
    stream.read_exact(&mut header).ok()?;
    let (length, _) = decode_header(&header)?;
    // a window tree is a few hundred KB at most
    if length > 16 * 1024 * 1024 {
        return None;
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// run one or more `;`/`,` separated commands. True when every one succeeded.
pub fn run_command(command: &str) -> bool {
    request(RUN_COMMAND, command)
        .and_then(|reply| {
            reply
                .as_array()
                .map(|results| results.iter().all(|r| r.get("success") == Some(&Value::Bool(true))))
        })
        .unwrap_or(false)
}

pub fn tree() -> Option<Value> {
    request(GET_TREE, "")
}

/// a window (a leaf container) of the layout tree
#[derive(Debug, Clone, PartialEq)]
pub struct TreeWindow {
    pub id: i64,
    pub pid: Option<u32>,
    /// Wayland app id (Sway) or X11 class (i3, XWayland)
    pub class: String,
    pub title: String,
    /// absolute (left, top, right, bottom)
    pub rect: (i32, i32, i32, i32),
    pub focused: bool,
    pub workspace: Option<String>,
}

fn rect_of(node: &Value) -> Option<(i32, i32, i32, i32)> {
    let rect = node.get("rect")?;
    let get = |key: &str| rect.get(key).and_then(Value::as_i64).map(|v| v as i32);
    let (x, y) = (get("x")?, get("y")?);
    Some((x, y, x + get("width")?, y + get("height")?))
}

fn children<'a>(node: &'a Value) -> impl Iterator<Item = &'a Value> {
    ["nodes", "floating_nodes"]
        .into_iter()
        .filter_map(|key| node.get(key).and_then(Value::as_array))
        .flatten()
}

fn collect_windows(node: &Value, workspace: Option<&str>, out: &mut Vec<TreeWindow>) {
    let workspace = if node.get("type").and_then(Value::as_str) == Some("workspace") {
        node.get("name").and_then(Value::as_str)
    } else {
        workspace
    };
    let has_children = children(node).next().is_some();
    let is_view = node.get("pid").is_some_and(|pid| !pid.is_null())
        || node.get("window").is_some_and(|window| !window.is_null());
    if !has_children && is_view {
        if let Some(rect) = rect_of(node) {
            let class = node
                .get("app_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    node.pointer("/window_properties/class").and_then(Value::as_str)
                })
                .unwrap_or_default()
                .to_string();
            out.push(TreeWindow {
                id: node.get("id").and_then(Value::as_i64).unwrap_or(0),
                pid: node.get("pid").and_then(Value::as_u64).map(|pid| pid as u32),
                class,
                title: node.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                rect,
                focused: node.get("focused").and_then(Value::as_bool).unwrap_or(false),
                workspace: workspace.map(str::to_string),
            });
        }
        return;
    }
    for child in children(node) {
        collect_windows(child, workspace, out);
    }
}

pub fn windows_from_tree(tree: &Value) -> Vec<TreeWindow> {
    let mut windows = Vec::new();
    collect_windows(tree, None, &mut windows);
    windows
}

/// output rectangles from the tree's `output` nodes (skipping i3's internal
/// `__i3` one)
pub fn outputs_from_tree(tree: &Value) -> Vec<(i32, i32, i32, i32)> {
    children(tree)
        .filter(|node| {
            node.get("type").and_then(Value::as_str) == Some("output")
                && node.get("name").and_then(Value::as_str) != Some("__i3")
        })
        .filter_map(rect_of)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Value {
        json!({
            "id": 1, "type": "root", "rect": {"x":0,"y":0,"width":3840,"height":1080},
            "nodes": [
                {"id": 2, "type": "output", "name": "__i3", "rect": {"x":0,"y":0,"width":3840,"height":1080}, "nodes": []},
                {"id": 3, "type": "output", "name": "DP-1", "rect": {"x":0,"y":0,"width":1920,"height":1080},
                 "nodes": [{
                    "id": 4, "type": "workspace", "name": "1", "rect": {"x":0,"y":0,"width":1920,"height":1080},
                    "nodes": [{
                        "id": 5, "type": "con", "pid": 4242, "name": "Rocket League (64-bit, DX11, Cooked)",
                        "focused": true, "app_id": null,
                        "window_properties": {"class": "steam_app_252950"},
                        "rect": {"x":0,"y":0,"width":1920,"height":1080}, "nodes": [], "floating_nodes": []
                    }],
                    "floating_nodes": [{
                        "id": 6, "type": "floating_con", "pid": 99, "name": "Hebnix", "focused": false,
                        "app_id": "Hebnix", "rect": {"x":100,"y":80,"width":1250,"height":800},
                        "nodes": [], "floating_nodes": []
                    }]
                 }]},
                {"id": 7, "type": "output", "name": "DP-2", "rect": {"x":1920,"y":0,"width":1920,"height":1080}, "nodes": []}
            ]
        })
    }

    #[test]
    fn frames_round_trip() {
        let message = encode(GET_TREE, "{}");
        let header: [u8; 14] = message[..14].try_into().unwrap();
        assert_eq!(decode_header(&header), Some((2, GET_TREE)));
        assert_eq!(&message[14..], b"{}");
        let mut bad = header;
        bad[0] = b'x';
        assert_eq!(decode_header(&bad), None);
    }

    #[test]
    fn finds_windows_with_workspace_focus_and_class() {
        let windows = windows_from_tree(&fixture());
        assert_eq!(windows.len(), 2);
        let rl = &windows[0];
        assert_eq!(rl.pid, Some(4242));
        assert!(rl.focused);
        assert_eq!(rl.class, "steam_app_252950");
        assert_eq!(rl.workspace.as_deref(), Some("1"));
        assert_eq!(rl.rect, (0, 0, 1920, 1080));
        let own = &windows[1];
        assert_eq!(own.class, "Hebnix");
        assert!(!own.focused);
        assert_eq!(own.rect, (100, 80, 1350, 880));
    }

    #[test]
    fn outputs_skip_the_internal_i3_output() {
        assert_eq!(
            outputs_from_tree(&fixture()),
            vec![(0, 0, 1920, 1080), (1920, 0, 3840, 1080)]
        );
    }
}
