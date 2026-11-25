//! Incremental line reader for an append-only file that may be mid-write.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub enum TailRead {
    Lines(Vec<String>),
    /// The file shrank or was replaced: lines re-read from offset 0.
    Reset(Vec<String>),
}

#[derive(Debug)]
pub struct Tail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    last_len: u64,
}

impl Tail {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            partial: Vec::new(),
            last_len: 0,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Cheap check: has the file size changed since the last read?
    pub fn changed(&self) -> bool {
        std::fs::metadata(&self.path)
            .map(|m| m.len() != self.last_len)
            .unwrap_or(false)
    }

    pub fn read_new(&mut self) -> std::io::Result<TailRead> {
        let mut f = File::open(&self.path)?;
        let len = f.metadata()?.len();
        let mut reset = false;
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
            reset = true;
        }
        self.last_len = len;
        if len == self.offset {
            return Ok(if reset {
                TailRead::Reset(vec![])
            } else {
                TailRead::Lines(vec![])
            });
        }
        f.seek(SeekFrom::Start(self.offset))?;
        let mut buf = std::mem::take(&mut self.partial);
        buf.reserve((len - self.offset) as usize);
        let n = f.take(len - self.offset).read_to_end(&mut buf)?;
        self.offset += n as u64;
        let mut lines = Vec::new();
        let mut from = 0;
        for (i, &b) in buf.iter().enumerate() {
            if b == b'\n' {
                let mut line = &buf[from..i];
                if line.last() == Some(&b'\r') {
                    line = &line[..line.len() - 1];
                }
                if !line.is_empty() {
                    lines.push(String::from_utf8_lossy(line).into_owned());
                }
                from = i + 1;
            }
        }
        self.partial = buf[from..].to_vec();
        Ok(if reset {
            TailRead::Reset(lines)
        } else {
            TailRead::Lines(lines)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn lines(r: TailRead) -> Vec<String> {
        match r {
            TailRead::Lines(l) => l,
            TailRead::Reset(_) => panic!("unexpected reset"),
        }
    }

    #[test]
    fn holds_partial_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        let mut f = File::create(&p).unwrap();
        write!(f, "{{\"a\":1}}\n{{\"b\"").unwrap();
        f.flush().unwrap();
        let mut t = Tail::new(p.clone());
        assert_eq!(lines(t.read_new().unwrap()), vec!["{\"a\":1}"]);
        write!(f, ":2}}\r\n").unwrap();
        f.flush().unwrap();
        assert!(t.changed());
        assert_eq!(lines(t.read_new().unwrap()), vec!["{\"b\":2}"]);
        assert!(!t.changed());
        assert!(lines(t.read_new().unwrap()).is_empty());
    }

    #[test]
    fn detects_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        std::fs::write(&p, "one\ntwo\n").unwrap();
        let mut t = Tail::new(p.clone());
        lines(t.read_new().unwrap());
        std::fs::write(&p, "x\n").unwrap();
        match t.read_new().unwrap() {
            TailRead::Reset(l) => assert_eq!(l, vec!["x"]),
            _ => panic!("expected reset"),
        }
    }
}
