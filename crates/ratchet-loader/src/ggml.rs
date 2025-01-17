use byteorder::{LittleEndian, ReadBytesExt};
use derive_new::new;
use ratchet::Shape;
use std::{
    collections::HashMap,
    io::{BufRead, Seek, SeekFrom},
    mem::MaybeUninit,
};

use crate::{GgmlDType, LoadError};

trait ReadBytesCustom: ReadBytesExt {
    /// Extends to read an exact number of bytes.
    fn read_bytes_with_len(&mut self, len: usize) -> std::io::Result<Vec<u8>> {
        let mut buf: Vec<MaybeUninit<u8>> = Vec::with_capacity(len);
        unsafe {
            buf.set_len(len);
        }
        let buf_slice = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, len) };
        self.read_exact(buf_slice)?;
        let buf = unsafe { std::mem::transmute::<_, Vec<u8>>(buf) };
        Ok(buf)
    }
}
impl<T: std::io::BufRead> ReadBytesCustom for T {}

pub const MAGIC_GGML: u32 = 0x67676d6c;
pub const MAGIC_GGJT: u32 = 0x67676a74;
pub const MAGIC_GGLA: u32 = 0x67676C61;
pub const MAGIC_GGMF: u32 = 0x67676d66;
pub const MAGIC_GGSN: u32 = 0x6767736e;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum GGMLFormat {
    GGML(u32),
    GGJT(u32, u32),
    GGLA(u32, u32),
    GGMF(u32, u32),
    GGSN(u32, u32),
}

impl GGMLFormat {
    pub fn read<R: BufRead + Seek>(reader: &mut R) -> Result<GGMLFormat, LoadError> {
        let magic = reader.read_u32::<byteorder::LittleEndian>()?;
        match magic {
            MAGIC_GGML => Ok(GGMLFormat::GGML(magic)),
            _ => {
                let version = reader.read_u32::<byteorder::LittleEndian>()?;
                match magic {
                    MAGIC_GGJT if (1..=3).contains(&version) => {
                        Ok(GGMLFormat::GGJT(magic, version))
                    }
                    MAGIC_GGLA if version == 1 => Ok(GGMLFormat::GGLA(magic, version)),
                    MAGIC_GGMF if version == 1 => Ok(GGMLFormat::GGMF(magic, version)),
                    _ => Err(LoadError::InvalidFormat(magic)),
                }
            }
        }
    }

    fn align32(&self) -> bool {
        match self {
            Self::GGML(_) => false,
            Self::GGJT(_, _) => true,
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TensorHeader {
    pub name: String,
    pub shape: Shape,
    pub dtype: GgmlDType,
    pub start_offset: u64,
    pub numel: usize,
}

impl TensorHeader {
    fn new(name: String, shape: Shape, dtype: GgmlDType, start_offset: u64) -> Self {
        let numel = shape.numel();
        Self {
            name,
            shape,
            dtype,
            start_offset,
            numel,
        }
    }

    fn data_size(&self) -> usize {
        self.numel * self.dtype.type_size() / self.dtype.block_size()
    }

    pub fn read_data<R: BufRead + Seek>(&self, reader: &mut R) -> std::io::Result<Vec<u8>> {
        let n_bytes = self.data_size();
        let mut buf: Vec<MaybeUninit<u8>> = Vec::with_capacity(n_bytes);
        unsafe {
            buf.set_len(n_bytes);
        }
        let buf_slice =
            unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, n_bytes) };

        reader.seek(SeekFrom::Start(self.start_offset))?;
        reader.read_exact(buf_slice)?;
        let buf = unsafe { std::mem::transmute::<_, Vec<u8>>(buf) };
        Ok(buf)
    }
}

#[derive(Debug, new)]
pub struct GGMLModel<M: GGMLCompatible> {
    pub header: M::ModelHeader,
    pub tensors: HashMap<String, TensorHeader>,
}

struct GGMLLoader;

impl GGMLLoader {
    pub fn load<R: BufRead + Seek, M: GGMLCompatible>(
        reader: &mut R,
    ) -> Result<GGMLModel<M>, LoadError> {
        let mut tensor_map = HashMap::new();
        let last_position = reader.seek(std::io::SeekFrom::End(0))?;
        reader.seek(std::io::SeekFrom::Start(0))?;
        let model_header = M::load_header(reader)?;

        while reader.stream_position()? != last_position {
            let header = Self::load_single(reader)?;
            tensor_map.insert(header.name.clone(), header);
        }
        Ok(GGMLModel::new(model_header, tensor_map))
    }

    fn load_single<R: BufRead + Seek>(reader: &mut R) -> Result<TensorHeader, LoadError> {
        let n_dims: usize = reader.read_i32::<LittleEndian>()?.try_into()?;
        let name_len = reader.read_i32::<LittleEndian>()?;
        let dtype = reader.read_u32::<LittleEndian>()?;

        let mut dims = vec![0u32; n_dims];
        reader.read_u32_into::<LittleEndian>(&mut dims)?;
        dims.reverse();

        let name = String::from_utf8(reader.read_bytes_with_len(name_len as _)?)?;
        let dtype = GgmlDType::try_from(dtype).map_err(|_| LoadError::UnsupportedDType {
            name: name.clone(),
            dtype,
        })?;

        let start_offset = reader.stream_position()?;
        let header = TensorHeader::new(name, dims.into(), dtype, start_offset);
        let data_size = header.data_size() as u64;
        reader.seek(SeekFrom::Start(start_offset + data_size))?;
        Ok(header)
    }
}

/// # GGML Compatible
///
/// Implement this for your Model if you want to load it from a GGML file.
pub trait GGMLCompatible: Sized {
    type ModelHeader;

    fn load_header<R: BufRead + Seek>(reader: &mut R) -> Result<Self::ModelHeader, LoadError>;
    fn load_ggml<R: BufRead + Seek>(reader: &mut R) -> Result<GGMLModel<Self>, LoadError> {
        GGMLLoader::load(reader)
    }
}
