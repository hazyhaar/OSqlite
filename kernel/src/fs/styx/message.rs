/// 9P2000 message types and wire format.
///
/// Each message is: size[4] type[1] tag[2] ... fields ...
/// Size includes itself (the 4 bytes).
use alloc::string::String;
use alloc::vec::Vec;

pub const NOTAG: u16 = 0xFFFF;
pub const NOFID: u32 = 0xFFFFFFFF;

/// 9P2000 message types.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StyxMsgType {
    Tversion = 100,
    Rversion = 101,
    Tauth = 102,
    Rauth = 103,
    Tattach = 104,
    Rattach = 105,
    Rerror = 107,
    Tflush = 108,
    Rflush = 109,
    Twalk = 110,
    Rwalk = 111,
    Topen = 112,
    Ropen = 113,
    Tcreate = 114,
    Rcreate = 115,
    Tread = 116,
    Rread = 117,
    Twrite = 118,
    Rwrite = 119,
    Tclunk = 120,
    Rclunk = 121,
    Tremove = 122,
    Rremove = 123,
    Tstat = 124,
    Rstat = 125,
    Twstat = 126,
    Rwstat = 127,
}

impl StyxMsgType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            100 => Some(Self::Tversion),
            101 => Some(Self::Rversion),
            102 => Some(Self::Tauth),
            103 => Some(Self::Rauth),
            104 => Some(Self::Tattach),
            105 => Some(Self::Rattach),
            107 => Some(Self::Rerror),
            108 => Some(Self::Tflush),
            109 => Some(Self::Rflush),
            110 => Some(Self::Twalk),
            111 => Some(Self::Rwalk),
            112 => Some(Self::Topen),
            113 => Some(Self::Ropen),
            114 => Some(Self::Tcreate),
            115 => Some(Self::Rcreate),
            116 => Some(Self::Tread),
            117 => Some(Self::Rread),
            118 => Some(Self::Twrite),
            119 => Some(Self::Rwrite),
            120 => Some(Self::Tclunk),
            121 => Some(Self::Rclunk),
            122 => Some(Self::Tremove),
            123 => Some(Self::Rremove),
            124 => Some(Self::Tstat),
            125 => Some(Self::Rstat),
            126 => Some(Self::Twstat),
            127 => Some(Self::Rwstat),
            _ => None,
        }
    }
}

/// Parsed 9P2000 message.
#[derive(Debug)]
pub enum StyxMsg {
    Tversion { tag: u16, msize: u32, version: String },
    Rversion { tag: u16, msize: u32, version: String },

    Tattach { tag: u16, fid: u32, afid: u32, uname: String, aname: String },
    Rattach { tag: u16, qid: Qid },

    Rerror { tag: u16, ename: String },

    Twalk { tag: u16, fid: u32, newfid: u32, wnames: Vec<String> },
    Rwalk { tag: u16, qids: Vec<Qid> },

    Topen { tag: u16, fid: u32, mode: u8 },
    Ropen { tag: u16, qid: Qid, iounit: u32 },

    Tread { tag: u16, fid: u32, offset: u64, count: u32 },
    Rread { tag: u16, data: Vec<u8> },

    Twrite { tag: u16, fid: u32, offset: u64, data: Vec<u8> },
    Rwrite { tag: u16, count: u32 },

    Tclunk { tag: u16, fid: u32 },
    Rclunk { tag: u16 },

    Tstat { tag: u16, fid: u32 },
    Rstat { tag: u16, stat: Stat },
}

/// 9P2000 Qid — unique identification of a file.
#[derive(Debug, Clone, Copy)]
pub struct Qid {
    pub qtype: u8,   // QTDIR=0x80, QTFILE=0x00
    pub version: u32,
    pub path: u64,
}

impl Qid {
    pub fn dir(path: u64) -> Self {
        Self { qtype: 0x80, version: 0, path }
    }

    pub fn file(path: u64) -> Self {
        Self { qtype: 0x00, version: 0, path }
    }

    /// Serialize to 13 bytes (wire format).
    pub fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(self.qtype);
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&self.path.to_le_bytes());
    }
}

/// 9P2000 Stat structure (simplified).
#[derive(Debug, Clone)]
pub struct Stat {
    pub qid: Qid,
    pub mode: u32,
    pub length: u64,
    pub name: String,
}

impl Stat {
    /// Serialize to wire format.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // stat[n] format: size[2] ... fields ...
        let name_bytes = self.name.as_bytes();

        // Placeholder for size (will fill in at the end)
        let size_pos = buf.len();
        buf.extend_from_slice(&[0u8; 2]); // stat size (excluding itself)

        // type[2] dev[4]
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());

        // qid[13]
        self.qid.encode(&mut buf);

        // mode[4]
        buf.extend_from_slice(&self.mode.to_le_bytes());

        // atime[4] mtime[4]
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());

        // length[8]
        buf.extend_from_slice(&self.length.to_le_bytes());

        // name[s]
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(name_bytes);

        // uid[s] gid[s] muid[s] — empty strings
        for _ in 0..3 {
            buf.extend_from_slice(&0u16.to_le_bytes());
        }

        // Fill in stat size
        let stat_size = (buf.len() - size_pos - 2) as u16;
        buf[size_pos..size_pos + 2].copy_from_slice(&stat_size.to_le_bytes());

        buf
    }
}

// ---- Wire format parsing ----

/// Parse a 9P2000 message from a byte buffer.
pub fn parse(data: &[u8]) -> Result<StyxMsg, ParseError> {
    if data.len() < 7 {
        return Err(ParseError::TooShort);
    }

    let size = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    if data.len() < size {
        return Err(ParseError::TooShort);
    }

    let msg_type = StyxMsgType::from_u8(data[4]).ok_or(ParseError::InvalidType)?;
    let tag = u16::from_le_bytes(data[5..7].try_into().unwrap());

    let body = &data[7..size];

    match msg_type {
        StyxMsgType::Tversion => {
            let msize = read_u32(body, 0)?;
            let version = read_string(body, 4)?;
            Ok(StyxMsg::Tversion { tag, msize, version })
        }
        StyxMsgType::Tattach => {
            let fid = read_u32(body, 0)?;
            let afid = read_u32(body, 4)?;
            let (uname, off) = read_string_off(body, 8)?;
            let aname = read_string(body, off)?;
            Ok(StyxMsg::Tattach { tag, fid, afid, uname, aname })
        }
        StyxMsgType::Twalk => {
            if body.len() < 10 {
                return Err(ParseError::TooShort);
            }
            let fid = read_u32(body, 0)?;
            let newfid = read_u32(body, 4)?;
            let nwname = u16::from_le_bytes(body[8..10].try_into().unwrap()) as usize;
            if nwname > 16 {
                return Err(ParseError::TooShort); // 9P2000 spec: max 16 walk elements
            }
            let mut wnames = Vec::with_capacity(nwname);
            let mut off = 10;
            for _ in 0..nwname {
                let (s, new_off) = read_string_off(body, off)?;
                wnames.push(s);
                off = new_off;
            }
            Ok(StyxMsg::Twalk { tag, fid, newfid, wnames })
        }
        StyxMsgType::Topen => {
            if body.len() < 5 {
                return Err(ParseError::TooShort);
            }
            let fid = read_u32(body, 0)?;
            let mode = body[4];
            Ok(StyxMsg::Topen { tag, fid, mode })
        }
        StyxMsgType::Tread => {
            if body.len() < 16 {
                return Err(ParseError::TooShort);
            }
            let fid = read_u32(body, 0)?;
            let offset = u64::from_le_bytes(body[4..12].try_into().unwrap());
            let count = read_u32(body, 12)?;
            Ok(StyxMsg::Tread { tag, fid, offset, count })
        }
        StyxMsgType::Twrite => {
            if body.len() < 16 {
                return Err(ParseError::TooShort);
            }
            let fid = read_u32(body, 0)?;
            let offset = u64::from_le_bytes(body[4..12].try_into().unwrap());
            let count = read_u32(body, 12)? as usize;
            if 16 + count > body.len() {
                return Err(ParseError::TooShort);
            }
            let data = body[16..16 + count].to_vec();
            Ok(StyxMsg::Twrite { tag, fid, offset, data })
        }
        StyxMsgType::Tclunk => {
            let fid = read_u32(body, 0)?;
            Ok(StyxMsg::Tclunk { tag, fid })
        }
        StyxMsgType::Tstat => {
            let fid = read_u32(body, 0)?;
            Ok(StyxMsg::Tstat { tag, fid })
        }
        _ => Err(ParseError::Unimplemented),
    }
}

/// Serialize a 9P2000 response message to bytes.
pub fn encode(msg: &StyxMsg) -> Vec<u8> {
    let mut buf = Vec::new();

    // Reserve 4 bytes for size
    buf.extend_from_slice(&[0u8; 4]);

    match msg {
        StyxMsg::Rversion { tag, msize, version } => {
            buf.push(StyxMsgType::Rversion as u8);
            buf.extend_from_slice(&tag.to_le_bytes());
            buf.extend_from_slice(&msize.to_le_bytes());
            write_string(&mut buf, version);
        }
        StyxMsg::Rattach { tag, qid } => {
            buf.push(StyxMsgType::Rattach as u8);
            buf.extend_from_slice(&tag.to_le_bytes());
            qid.encode(&mut buf);
        }
        StyxMsg::Rerror { tag, ename } => {
            buf.push(StyxMsgType::Rerror as u8);
            buf.extend_from_slice(&tag.to_le_bytes());
            write_string(&mut buf, ename);
        }
        StyxMsg::Rwalk { tag, qids } => {
            buf.push(StyxMsgType::Rwalk as u8);
            buf.extend_from_slice(&tag.to_le_bytes());
            buf.extend_from_slice(&(qids.len() as u16).to_le_bytes());
            for qid in qids {
                qid.encode(&mut buf);
            }
        }
        StyxMsg::Ropen { tag, qid, iounit } => {
            buf.push(StyxMsgType::Ropen as u8);
            buf.extend_from_slice(&tag.to_le_bytes());
            qid.encode(&mut buf);
            buf.extend_from_slice(&iounit.to_le_bytes());
        }
        StyxMsg::Rread { tag, data } => {
            buf.push(StyxMsgType::Rread as u8);
            buf.extend_from_slice(&tag.to_le_bytes());
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buf.extend_from_slice(data);
        }
        StyxMsg::Rwrite { tag, count } => {
            buf.push(StyxMsgType::Rwrite as u8);
            buf.extend_from_slice(&tag.to_le_bytes());
            buf.extend_from_slice(&count.to_le_bytes());
        }
        StyxMsg::Rclunk { tag } => {
            buf.push(StyxMsgType::Rclunk as u8);
            buf.extend_from_slice(&tag.to_le_bytes());
        }
        StyxMsg::Rstat { tag, stat } => {
            buf.push(StyxMsgType::Rstat as u8);
            buf.extend_from_slice(&tag.to_le_bytes());
            let stat_data = stat.encode();
            // Rstat wraps stat in another size[2] prefix
            buf.extend_from_slice(&(stat_data.len() as u16).to_le_bytes());
            buf.extend_from_slice(&stat_data);
        }
        _ => {} // T-messages are not encoded by the server
    }

    // Fill in total size
    let size = buf.len() as u32;
    buf[0..4].copy_from_slice(&size.to_le_bytes());

    buf
}

// ---- Helpers ----

#[derive(Debug)]
pub enum ParseError {
    TooShort,
    InvalidType,
    Unimplemented,
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, ParseError> {
    if offset + 4 > data.len() {
        return Err(ParseError::TooShort);
    }
    Ok(u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()))
}

fn read_string(data: &[u8], offset: usize) -> Result<String, ParseError> {
    let (s, _) = read_string_off(data, offset)?;
    Ok(s)
}

fn read_string_off(data: &[u8], offset: usize) -> Result<(String, usize), ParseError> {
    if offset + 2 > data.len() {
        return Err(ParseError::TooShort);
    }
    let len = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
    let str_start = offset + 2;
    if str_start + len > data.len() {
        return Err(ParseError::TooShort);
    }
    let s = String::from_utf8_lossy(&data[str_start..str_start + len]).into_owned();
    Ok((s, str_start + len))
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u16).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}
