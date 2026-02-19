/// Styx server — handles 9P2000 requests against the synthetic namespace.
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::message::{self, StyxMsg, Qid, Stat};
use super::namespace::Node;

/// Maximum message size negotiated in Tversion.
const MAX_MSIZE: u32 = 65536;

/// A fid tracks an open reference to a node in the namespace.
struct Fid {
    /// Path from root to reach this node (for walking).
    path: Vec<String>,
    /// Is this fid open for I/O?
    open: bool,
}

/// The Styx server: processes 9P2000 messages against a namespace.
pub struct StyxServer {
    root: Node,
    fids: BTreeMap<u32, Fid>,
    msize: u32,
}

impl StyxServer {
    pub fn new(root: Node) -> Self {
        Self {
            root,
            fids: BTreeMap::new(),
            msize: MAX_MSIZE,
        }
    }

    /// Process a raw 9P2000 message buffer and return the response bytes.
    pub fn handle_message(&mut self, data: &[u8]) -> Vec<u8> {
        match message::parse(data) {
            Ok(msg) => {
                let response = self.dispatch(msg);
                message::encode(&response)
            }
            Err(_) => {
                let err = StyxMsg::Rerror {
                    tag: 0,
                    ename: String::from("parse error"),
                };
                message::encode(&err)
            }
        }
    }

    /// Dispatch a parsed message to the appropriate handler.
    fn dispatch(&mut self, msg: StyxMsg) -> StyxMsg {
        match msg {
            StyxMsg::Tversion { tag, msize, version } => {
                self.msize = msize.min(MAX_MSIZE);
                let ver = if version.starts_with("9P2000") {
                    String::from("9P2000")
                } else {
                    String::from("unknown")
                };
                StyxMsg::Rversion {
                    tag,
                    msize: self.msize,
                    version: ver,
                }
            }

            StyxMsg::Tattach { tag, fid, .. } => {
                self.fids.insert(fid, Fid {
                    path: Vec::new(), // root
                    open: false,
                });
                StyxMsg::Rattach {
                    tag,
                    qid: Qid::dir(self.root.path_id),
                }
            }

            StyxMsg::Twalk { tag, fid, newfid, wnames } => {
                let base_path = match self.fids.get(&fid) {
                    Some(f) => f.path.clone(),
                    None => return self.error(tag, "unknown fid"),
                };

                let mut current_path = base_path;
                let mut qids = Vec::new();

                for name in &wnames {
                    current_path.push(name.clone());
                    match self.resolve_path(&current_path) {
                        Some(node) => {
                            let qid = if node.is_dir() {
                                Qid::dir(node.path_id)
                            } else {
                                Qid::file(node.path_id)
                            };
                            qids.push(qid);
                        }
                        None => return self.error(tag, "file not found"),
                    }
                }

                self.fids.insert(newfid, Fid {
                    path: current_path,
                    open: false,
                });

                StyxMsg::Rwalk { tag, qids }
            }

            StyxMsg::Topen { tag, fid, .. } => {
                let node = match self.fid_to_node(&fid) {
                    Some(n) => n,
                    None => return self.error(tag, "unknown fid"),
                };

                let qid = if node.is_dir() {
                    Qid::dir(node.path_id)
                } else {
                    Qid::file(node.path_id)
                };

                if let Some(f) = self.fids.get_mut(&fid) {
                    f.open = true;
                }

                StyxMsg::Ropen {
                    tag,
                    qid,
                    iounit: self.msize - 24, // max payload
                }
            }

            StyxMsg::Tread { tag, fid, offset, count } => {
                let node = match self.fid_to_node(&fid) {
                    Some(n) => n,
                    None => return self.error(tag, "unknown fid"),
                };

                let content = node.read();
                let offset = offset as usize;
                let count = count as usize;

                let data = if offset >= content.len() {
                    Vec::new()
                } else {
                    let end = (offset + count).min(content.len());
                    content[offset..end].to_vec()
                };

                StyxMsg::Rread { tag, data }
            }

            StyxMsg::Twrite { tag, fid, data, .. } => {
                let node = match self.fid_to_node_mut(&fid) {
                    Some(n) => n,
                    None => return self.error(tag, "unknown fid"),
                };

                match node.write(&data) {
                    Ok(()) => StyxMsg::Rwrite { tag, count: data.len() as u32 },
                    Err(e) => self.error(tag, &e),
                }
            }

            StyxMsg::Tclunk { tag, fid } => {
                self.fids.remove(&fid);
                StyxMsg::Rclunk { tag }
            }

            StyxMsg::Tstat { tag, fid } => {
                let node = match self.fid_to_node(&fid) {
                    Some(n) => n,
                    None => return self.error(tag, "unknown fid"),
                };

                let mode = if node.is_dir() { 0x80000000 | 0o755 } else { 0o644 };
                let length = if node.is_dir() { 0 } else { node.read().len() as u64 };
                let qid = if node.is_dir() {
                    Qid::dir(node.path_id)
                } else {
                    Qid::file(node.path_id)
                };

                StyxMsg::Rstat {
                    tag,
                    stat: Stat {
                        qid,
                        mode,
                        length,
                        name: node.name.clone(),
                    },
                }
            }

            _ => self.error(0, "unhandled message type"),
        }
    }

    /// Resolve a path (list of names) to a node in the namespace.
    fn resolve_path(&self, path: &[String]) -> Option<&Node> {
        let mut current = &self.root;
        for component in path {
            current = current.child(component)?;
        }
        Some(current)
    }

    fn resolve_path_mut(&mut self, path: &[String]) -> Option<&mut Node> {
        let mut current = &mut self.root;
        for component in path {
            current = current.child_mut(component)?;
        }
        Some(current)
    }

    fn fid_to_node(&self, fid: &u32) -> Option<&Node> {
        let f = self.fids.get(fid)?;
        self.resolve_path(&f.path)
    }

    fn fid_to_node_mut(&mut self, fid: &u32) -> Option<&mut Node> {
        let path = self.fids.get(fid)?.path.clone();
        self.resolve_path_mut(&path)
    }

    fn error(&self, tag: u16, msg: &str) -> StyxMsg {
        StyxMsg::Rerror {
            tag,
            ename: String::from(msg),
        }
    }
}
