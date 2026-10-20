//! AMF0 (Action Message Format) — the encoding of RTMP commands and stream metadata.

#[derive(Clone, Debug, PartialEq)]
pub enum Amf {
    Number(f64),
    Bool(bool),
    String(String),
    Object(Vec<(String, Amf)>),
    Null,
    Undefined,
    EcmaArray(Vec<(String, Amf)>),
    StrictArray(Vec<Amf>),
}

impl Amf {
    pub fn str(s: &str) -> Amf {
        Amf::String(s.to_string())
    }

    pub fn obj(fields: &[(&str, Amf)]) -> Amf {
        Amf::Object(fields.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Amf::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Amf::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// A field of an object / ECMA array.
    pub fn get(&self, key: &str) -> Option<&Amf> {
        match self {
            Amf::Object(f) | Amf::EcmaArray(f) => f.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Amf::Number(n) => {
                out.push(0x00);
                out.extend_from_slice(&n.to_be_bytes());
            }
            Amf::Bool(b) => {
                out.push(0x01);
                out.push(*b as u8);
            }
            Amf::String(s) => {
                if s.len() > 0xFFFF {
                    out.push(0x0C);
                    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
                } else {
                    out.push(0x02);
                    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
                }
                out.extend_from_slice(s.as_bytes());
            }
            Amf::Object(fields) => {
                out.push(0x03);
                encode_props(fields, out);
            }
            Amf::Null => out.push(0x05),
            Amf::Undefined => out.push(0x06),
            Amf::EcmaArray(fields) => {
                out.push(0x08);
                out.extend_from_slice(&(fields.len() as u32).to_be_bytes());
                encode_props(fields, out);
            }
            Amf::StrictArray(items) => {
                out.push(0x0A);
                out.extend_from_slice(&(items.len() as u32).to_be_bytes());
                for i in items {
                    i.encode(out);
                }
            }
        }
    }
}

fn encode_props(fields: &[(String, Amf)], out: &mut Vec<u8>) {
    for (k, v) in fields {
        out.extend_from_slice(&(k.len() as u16).to_be_bytes());
        out.extend_from_slice(k.as_bytes());
        v.encode(out);
    }
    out.extend_from_slice(&[0, 0, 0x09]);
}

pub fn encode_all(values: &[Amf]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        v.encode(&mut out);
    }
    out
}

/// Decode every value in `data` (stops quietly at anything it doesn't understand).
pub fn decode_all(data: &[u8]) -> Vec<Amf> {
    let mut r = Reader { d: data, p: 0 };
    let mut out = Vec::new();
    while r.p < data.len() {
        match r.value() {
            Some(v) => out.push(v),
            None => break,
        }
    }
    out
}

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.d.get(self.p..self.p + n)?;
        self.p += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn string(&mut self, n: usize) -> Option<String> {
        self.take(n).map(|b| String::from_utf8_lossy(b).into_owned())
    }
    fn props(&mut self) -> Option<Vec<(String, Amf)>> {
        let mut f = Vec::new();
        loop {
            let n = self.u16()? as usize;
            if n == 0 {
                // end marker 0x09 (tolerate its absence)
                if self.d.get(self.p) == Some(&0x09) {
                    self.p += 1;
                }
                return Some(f);
            }
            let k = self.string(n)?;
            let v = self.value()?;
            f.push((k, v));
        }
    }
    fn value(&mut self) -> Option<Amf> {
        Some(match self.u8()? {
            0x00 => {
                let b = self.take(8)?;
                Amf::Number(f64::from_be_bytes(b.try_into().ok()?))
            }
            0x01 => Amf::Bool(self.u8()? != 0),
            0x02 => {
                let n = self.u16()? as usize;
                Amf::String(self.string(n)?)
            }
            0x03 => Amf::Object(self.props()?),
            0x05 => Amf::Null,
            0x06 => Amf::Undefined,
            0x08 => {
                self.u32()?;
                Amf::EcmaArray(self.props()?)
            }
            0x0A => {
                let n = self.u32()? as usize;
                let mut v = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    v.push(self.value()?);
                }
                Amf::StrictArray(v)
            }
            0x0B => {
                self.take(10)?;
                Amf::Null // date
            }
            0x0C => {
                let n = self.u32()? as usize;
                Amf::String(self.string(n)?)
            }
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let v = vec![
            Amf::str("connect"),
            Amf::Number(1.0),
            Amf::obj(&[("app", Amf::str("live")), ("flag", Amf::Bool(true))]),
            Amf::Null,
            Amf::EcmaArray(vec![("width".into(), Amf::Number(1280.0))]),
        ];
        assert_eq!(decode_all(&encode_all(&v)), v);
    }
}
