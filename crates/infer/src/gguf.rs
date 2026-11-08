//! GGUF: llama.cpp's model file format. A header, key/value metadata, tensor descriptions, then
//! the tensor data, aligned. The file is memory-mapped: on a phone the weights stay in the page
//! cache and are never copied into the heap.
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    F32(f32),
    F64(f64),
    Bool(bool),
    Str(String),
    Array(Vec<Value>),
}

impl Value {
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::U8(v) => Some(v as u64),
            Value::U16(v) => Some(v as u64),
            Value::U32(v) => Some(v as u64),
            Value::U64(v) => Some(v),
            Value::I8(v) if v >= 0 => Some(v as u64),
            Value::I16(v) if v >= 0 => Some(v as u64),
            Value::I32(v) if v >= 0 => Some(v as u64),
            Value::I64(v) if v >= 0 => Some(v as u64),
            _ => None,
        }
    }
    pub fn as_f32(&self) -> Option<f32> {
        match *self {
            Value::F32(v) => Some(v),
            Value::F64(v) => Some(v as f32),
            _ => self.as_u64().map(|v| v as f32),
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
}

/// ggml tensor types (the ones that matter here; others are recognised but not computed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    F32,
    F16,
    BF16,
    Q4_0,
    Q5_0,
    Q8_0,
    Q4K,
    Q5K,
    Q6K,
    Other(u32),
}

impl DType {
    fn from(id: u32) -> DType {
        match id {
            0 => DType::F32,
            1 => DType::F16,
            2 => DType::Q4_0,
            6 => DType::Q5_0,
            8 => DType::Q8_0,
            12 => DType::Q4K,
            13 => DType::Q5K,
            14 => DType::Q6K,
            30 => DType::BF16,
            n => DType::Other(n),
        }
    }
    /// (elements per block, bytes per block)
    pub fn block(self) -> Option<(usize, usize)> {
        Some(match self {
            DType::F32 => (1, 4),
            DType::F16 | DType::BF16 => (1, 2),
            DType::Q4_0 => (32, 18),
            DType::Q5_0 => (32, 22),
            DType::Q8_0 => (32, 34),
            DType::Q4K => (256, 144),
            DType::Q5K => (256, 176),
            DType::Q6K => (256, 210),
            DType::Other(_) => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TensorInfo {
    pub name: String,
    /// ggml order: dims[0] is the fastest-moving (a row's length).
    pub dims: Vec<u64>,
    pub dtype: DType,
    /// Byte offset from the start of the file.
    pub offset: usize,
    pub size: usize,
}

impl TensorInfo {
    pub fn elements(&self) -> u64 {
        self.dims.iter().product()
    }
}

pub struct Gguf {
    pub version: u32,
    pub meta: BTreeMap<String, Value>,
    pub tensors: Vec<TensorInfo>,
    map: memmap2::Mmap,
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self
            .at
            .checked_add(n)
            .filter(|e| *e <= self.b.len())
            .ok_or("GGUF file is cut short")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String, String> {
        let n = self.u64()? as usize;
        if n > 1 << 24 {
            return Err("GGUF string too long".into());
        }
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn value(&mut self, ty: u32) -> Result<Value, String> {
        Ok(match ty {
            0 => Value::U8(self.take(1)?[0]),
            1 => Value::I8(self.take(1)?[0] as i8),
            2 => Value::U16(u16::from_le_bytes(self.take(2)?.try_into().unwrap())),
            3 => Value::I16(i16::from_le_bytes(self.take(2)?.try_into().unwrap())),
            4 => Value::U32(self.u32()?),
            5 => Value::I32(self.u32()? as i32),
            6 => Value::F32(f32::from_bits(self.u32()?)),
            7 => Value::Bool(self.take(1)?[0] != 0),
            8 => Value::Str(self.string()?),
            9 => {
                let inner = self.u32()?;
                let n = self.u64()? as usize;
                if n > 1 << 24 {
                    return Err("GGUF array too long".into());
                }
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.value(inner)?);
                }
                Value::Array(v)
            }
            10 => Value::U64(self.u64()?),
            11 => Value::I64(self.u64()? as i64),
            12 => Value::F64(f64::from_bits(self.u64()?)),
            t => return Err(format!("unknown GGUF value type {t}")),
        })
    }
}

impl Gguf {
    pub fn open(path: &Path) -> Result<Gguf, String> {
        let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        // Safety: the file is opened read-only and not modified while mapped.
        let map = unsafe { memmap2::Mmap::map(&f) }.map_err(|e| e.to_string())?;
        let mut r = Reader { b: &map, at: 0 };
        if r.take(4)? != b"GGUF" {
            return Err("not a GGUF file".into());
        }
        let version = r.u32()?;
        if !(2..=3).contains(&version) {
            return Err(format!("GGUF version {version} is not supported"));
        }
        let n_tensors = r.u64()? as usize;
        let n_kv = r.u64()? as usize;
        let mut meta = BTreeMap::new();
        for _ in 0..n_kv {
            let k = r.string()?;
            let ty = r.u32()?;
            meta.insert(k, r.value(ty)?);
        }
        let mut infos = Vec::with_capacity(n_tensors);
        for _ in 0..n_tensors {
            let name = r.string()?;
            let nd = r.u32()? as usize;
            let mut dims = Vec::with_capacity(nd);
            for _ in 0..nd {
                dims.push(r.u64()?);
            }
            let dtype = DType::from(r.u32()?);
            let rel = r.u64()? as usize;
            infos.push((name, dims, dtype, rel));
        }
        let align = meta
            .get("general.alignment")
            .and_then(Value::as_u64)
            .unwrap_or(32) as usize;
        let data_start = r.at.div_ceil(align) * align;
        let mut tensors = Vec::with_capacity(n_tensors);
        for (name, dims, dtype, rel) in infos {
            let elems: u64 = dims.iter().product();
            let size = match dtype.block() {
                Some((per, bytes)) => (elems as usize).div_ceil(per) * bytes,
                None => 0,
            };
            let offset = data_start + rel;
            if offset + size > map.len() {
                return Err(format!("tensor {name} runs past the end of the file"));
            }
            tensors.push(TensorInfo {
                name,
                dims,
                dtype,
                offset,
                size,
            });
        }
        Ok(Gguf {
            version,
            meta,
            tensors,
            map,
        })
    }

    /// Keep the weights in RAM (mlock), so the model never pages back out to flash. Needs
    /// privileges or a high enough RLIMIT_MEMLOCK; returns the error otherwise.
    pub fn lock(&self) -> Result<(), String> {
        #[cfg(unix)]
        {
            let r =
                unsafe { libc::mlock(self.map.as_ptr() as *const libc::c_void, self.map.len()) };
            if r != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
        Ok(())
    }

    pub fn tensor(&self, name: &str) -> Option<&TensorInfo> {
        self.tensors.iter().find(|t| t.name == name)
    }
    /// The raw bytes of a tensor, straight from the mapped file.
    pub fn bytes(&self, t: &TensorInfo) -> &[u8] {
        &self.map[t.offset..t.offset + t.size]
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.meta.get(key)
    }
    pub fn arch(&self) -> &str {
        self.get("general.architecture")
            .and_then(Value::as_str)
            .unwrap_or("")
    }
    /// `<arch>.<key>`, e.g. `qwen3.block_count`.
    pub fn arch_u64(&self, key: &str) -> Option<u64> {
        self.get(&format!("{}.{key}", self.arch()))
            .and_then(Value::as_u64)
    }
    pub fn arch_f32(&self, key: &str) -> Option<f32> {
        self.get(&format!("{}.{key}", self.arch()))
            .and_then(Value::as_f32)
    }
}
