//! Module format — see nlvm-specs/docs/vm.md § Module format.

use crate::constant_pool::{ConstantPool, ConstantPoolEntry};
use crate::error::BytecodeError;
use sha2::{Digest, Sha256};

pub const MAGIC: u32 = 0x4E4C_4D00;
pub const VERSION: u16 = 2;

pub mod class_flags {
    pub const READONLY: u16 = 1 << 0;
    pub const INTERFACE: u16 = 1 << 1;
    pub const ENUM: u16 = 1 << 2;
    pub const ABSTRACT: u16 = 1 << 3;
    pub const FINAL: u16 = 1 << 4;
}

pub mod field_flags {
    pub const PUBLIC: u16 = 1 << 0;
    pub const PROTECTED: u16 = 1 << 1;
    pub const PRIVATE: u16 = 1 << 2;
    pub const STATIC: u16 = 1 << 3;
    pub const READONLY: u16 = 1 << 4;
}

pub mod method_flags {
    pub const PUBLIC: u16 = 1 << 0;
    pub const PROTECTED: u16 = 1 << 1;
    pub const PRIVATE: u16 = 1 << 2;
    pub const STATIC: u16 = 1 << 3;
    pub const CONST: u16 = 1 << 4;
    pub const NODISCARD: u16 = 1 << 5;
    pub const CONSTRUCTOR: u16 = 1 << 6;
    pub const DESTRUCTOR: u16 = 1 << 7;
    pub const ABSTRACT: u16 = 1 << 8;
    pub const FINAL: u16 = 1 << 9;
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDescriptor {
    pub flags: u16,
    pub name_index: u16,
    pub type_index: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExceptionTableEntry {
    pub start_pc: u16,
    pub end_pc: u16,
    pub handler_pc: u16,
    /// Constant pool `CLASS` index of the caught type, or `0` for catch-all.
    pub catch_type: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LineTableEntry {
    pub start_pc: u16,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodDescriptor {
    pub flags: u16,
    pub name_index: u16,
    pub descriptor_index: u16,
    pub throws_types: Vec<u16>,
    pub max_locals: u16,
    pub max_stack: u16,
    pub code: Vec<u8>,
    pub exception_table: Vec<ExceptionTableEntry>,
    pub line_table: Vec<LineTableEntry>,
}

impl MethodDescriptor {
    pub fn is_static(&self) -> bool {
        self.flags & method_flags::STATIC != 0
    }
}

/// Integrity trailer hash algorithm — see spec § Module integrity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgo {
    None,
    Sha256,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub version: u16,
    pub constant_pool: ConstantPool,
    pub this_class: u16,
    pub class_flags: u16,
    pub super_class: u16,
    pub interfaces: Vec<u16>,
    pub fields: Vec<FieldDescriptor>,
    pub methods: Vec<MethodDescriptor>,
    pub hash_algo: HashAlgo,
}

impl Module {
    pub fn this_class_name(&self) -> Option<&str> {
        self.constant_pool.class_name_at(self.this_class)
    }

    pub fn find_method(&self, name: &str) -> Option<&MethodDescriptor> {
        self.methods.iter().find(|m| {
            self.constant_pool.utf8_at(m.name_index) == Some(name)
        })
    }

    pub fn find_field(&self, name: &str) -> Option<&FieldDescriptor> {
        self.fields
            .iter()
            .find(|f| self.constant_pool.utf8_at(f.name_index) == Some(name))
    }

    /// Like `find_method`, but also matches on the method descriptor string
    /// (e.g. `"(int, string) -> void"`) — needed once a name can resolve to
    /// several overloads (constructors, overloaded instance methods).
    pub fn find_method_by_descriptor(&self, name: &str, descriptor: &str) -> Option<&MethodDescriptor> {
        self.methods.iter().find(|m| {
            self.constant_pool.utf8_at(m.name_index) == Some(name)
                && self.constant_pool.type_desc_at(m.descriptor_index) == Some(descriptor)
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.extend_from_slice(&self.version.to_be_bytes());

        let cp_entries = self.constant_pool.entries();
        buf.extend_from_slice(&(cp_entries.len() as u16 + 1).to_be_bytes());
        for entry in cp_entries {
            write_cp_entry(&mut buf, entry);
        }

        buf.extend_from_slice(&self.this_class.to_be_bytes());
        buf.extend_from_slice(&self.class_flags.to_be_bytes());
        buf.extend_from_slice(&self.super_class.to_be_bytes());

        buf.extend_from_slice(&(self.interfaces.len() as u16).to_be_bytes());
        for i in &self.interfaces {
            buf.extend_from_slice(&i.to_be_bytes());
        }

        buf.extend_from_slice(&(self.fields.len() as u16).to_be_bytes());
        for f in &self.fields {
            buf.extend_from_slice(&f.flags.to_be_bytes());
            buf.extend_from_slice(&f.name_index.to_be_bytes());
            buf.extend_from_slice(&f.type_index.to_be_bytes());
        }

        buf.extend_from_slice(&(self.methods.len() as u16).to_be_bytes());
        for m in &self.methods {
            buf.extend_from_slice(&m.flags.to_be_bytes());
            buf.extend_from_slice(&m.name_index.to_be_bytes());
            buf.extend_from_slice(&m.descriptor_index.to_be_bytes());
            buf.extend_from_slice(&(m.throws_types.len() as u16).to_be_bytes());
            for t in &m.throws_types {
                buf.extend_from_slice(&t.to_be_bytes());
            }
            buf.extend_from_slice(&m.max_locals.to_be_bytes());
            buf.extend_from_slice(&m.max_stack.to_be_bytes());
            buf.extend_from_slice(&(m.code.len() as u32).to_be_bytes());
            buf.extend_from_slice(&m.code);
            buf.extend_from_slice(&(m.exception_table.len() as u16).to_be_bytes());
            for e in &m.exception_table {
                buf.extend_from_slice(&e.start_pc.to_be_bytes());
                buf.extend_from_slice(&e.end_pc.to_be_bytes());
                buf.extend_from_slice(&e.handler_pc.to_be_bytes());
                buf.extend_from_slice(&e.catch_type.to_be_bytes());
            }
            buf.extend_from_slice(&(m.line_table.len() as u16).to_be_bytes());
            for l in &m.line_table {
                buf.extend_from_slice(&l.start_pc.to_be_bytes());
                buf.extend_from_slice(&l.line.to_be_bytes());
            }
        }

        match self.hash_algo {
            HashAlgo::None => buf.push(0),
            HashAlgo::Sha256 => {
                let digest = Sha256::digest(&buf);
                buf.push(1);
                buf.extend_from_slice(&digest);
            }
        }

        buf
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, BytecodeError> {
        let mut r = Reader::new(bytes);

        let magic = r.read_u32()?;
        if magic != MAGIC {
            return Err(BytecodeError::BadMagic(magic));
        }
        let version = r.read_u16()?;

        let cp_count = r.read_u16()?;
        let mut constant_pool = ConstantPool::new();
        for _ in 0..cp_count.saturating_sub(1) {
            let entry = read_cp_entry(&mut r)?;
            match entry {
                ConstantPoolEntry::Int(v) => {
                    constant_pool.add_int(v);
                }
                ConstantPoolEntry::Float(v) => {
                    constant_pool.add_float(v);
                }
                ConstantPoolEntry::Utf8(s) => {
                    constant_pool.add_utf8(s);
                }
                ConstantPoolEntry::Class { name_index } => {
                    let name = constant_pool
                        .utf8_at(name_index)
                        .ok_or(BytecodeError::Malformed("class name_index"))?
                        .to_string();
                    constant_pool.add_class(&name);
                }
                ConstantPoolEntry::FieldRef {
                    class_index,
                    name_index,
                    type_index,
                } => {
                    constant_pool.add_field_ref(class_index, name_index, type_index);
                }
                ConstantPoolEntry::MethodRef {
                    class_index,
                    name_index,
                    descriptor_index,
                } => {
                    constant_pool.add_method_ref(class_index, name_index, descriptor_index);
                }
                ConstantPoolEntry::TypeDesc { desc_index } => {
                    let desc = constant_pool
                        .utf8_at(desc_index)
                        .ok_or(BytecodeError::Malformed("type desc_index"))?
                        .to_string();
                    constant_pool.add_type_desc(&desc);
                }
            }
        }

        let this_class = r.read_u16()?;
        let class_flags = r.read_u16()?;
        let super_class = r.read_u16()?;

        let interfaces_count = r.read_u16()?;
        let mut interfaces = Vec::with_capacity(interfaces_count as usize);
        for _ in 0..interfaces_count {
            interfaces.push(r.read_u16()?);
        }

        let fields_count = r.read_u16()?;
        let mut fields = Vec::with_capacity(fields_count as usize);
        for _ in 0..fields_count {
            fields.push(FieldDescriptor {
                flags: r.read_u16()?,
                name_index: r.read_u16()?,
                type_index: r.read_u16()?,
            });
        }

        let methods_count = r.read_u16()?;
        let mut methods = Vec::with_capacity(methods_count as usize);
        for _ in 0..methods_count {
            let flags = r.read_u16()?;
            let name_index = r.read_u16()?;
            let descriptor_index = r.read_u16()?;
            let throws_count = r.read_u16()?;
            let mut throws_types = Vec::with_capacity(throws_count as usize);
            for _ in 0..throws_count {
                throws_types.push(r.read_u16()?);
            }
            let max_locals = r.read_u16()?;
            let max_stack = r.read_u16()?;
            let code_length = r.read_u32()?;
            let code = r.read_bytes(code_length as usize)?.to_vec();
            let exception_table_count = r.read_u16()?;
            let mut exception_table = Vec::with_capacity(exception_table_count as usize);
            for _ in 0..exception_table_count {
                exception_table.push(ExceptionTableEntry {
                    start_pc: r.read_u16()?,
                    end_pc: r.read_u16()?,
                    handler_pc: r.read_u16()?,
                    catch_type: r.read_u16()?,
                });
            }
            let line_table_count = r.read_u16()?;
            let mut line_table = Vec::with_capacity(line_table_count as usize);
            for _ in 0..line_table_count {
                line_table.push(LineTableEntry {
                    start_pc: r.read_u16()?,
                    line: r.read_u32()?,
                });
            }
            methods.push(MethodDescriptor {
                flags,
                name_index,
                descriptor_index,
                throws_types,
                max_locals,
                max_stack,
                code,
                exception_table,
                line_table,
            });
        }

        let preceding_len = r.offset();
        let hash_algo_byte = r.read_u8()?;
        let hash_algo = match hash_algo_byte {
            0 => HashAlgo::None,
            1 => HashAlgo::Sha256,
            other => return Err(BytecodeError::UnknownHashAlgo(other)),
        };
        if hash_algo == HashAlgo::Sha256 {
            let hash = r.read_bytes(32)?;
            let expected = Sha256::digest(&bytes[..preceding_len]);
            if hash != expected.as_slice() {
                return Err(BytecodeError::HashMismatch);
            }
        }

        Ok(Module {
            version,
            constant_pool,
            this_class,
            class_flags,
            super_class,
            interfaces,
            fields,
            methods,
            hash_algo,
        })
    }
}

fn write_cp_entry(buf: &mut Vec<u8>, entry: &ConstantPoolEntry) {
    buf.push(entry.tag());
    match entry {
        ConstantPoolEntry::Int(v) => buf.extend_from_slice(&v.to_be_bytes()),
        ConstantPoolEntry::Float(v) => buf.extend_from_slice(&v.to_be_bytes()),
        ConstantPoolEntry::Utf8(s) => {
            let bytes = s.as_bytes();
            buf.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            buf.extend_from_slice(bytes);
        }
        ConstantPoolEntry::Class { name_index } => buf.extend_from_slice(&name_index.to_be_bytes()),
        ConstantPoolEntry::FieldRef {
            class_index,
            name_index,
            type_index,
        } => {
            buf.extend_from_slice(&class_index.to_be_bytes());
            buf.extend_from_slice(&name_index.to_be_bytes());
            buf.extend_from_slice(&type_index.to_be_bytes());
        }
        ConstantPoolEntry::MethodRef {
            class_index,
            name_index,
            descriptor_index,
        } => {
            buf.extend_from_slice(&class_index.to_be_bytes());
            buf.extend_from_slice(&name_index.to_be_bytes());
            buf.extend_from_slice(&descriptor_index.to_be_bytes());
        }
        ConstantPoolEntry::TypeDesc { desc_index } => buf.extend_from_slice(&desc_index.to_be_bytes()),
    }
}

fn read_cp_entry(r: &mut Reader) -> Result<ConstantPoolEntry, BytecodeError> {
    let tag = r.read_u8()?;
    let entry = match tag {
        1 => ConstantPoolEntry::Int(r.read_i64()?),
        2 => ConstantPoolEntry::Float(r.read_f64()?),
        3 => {
            let len = r.read_u16()? as usize;
            let bytes = r.read_bytes(len)?;
            let s = std::str::from_utf8(bytes)
                .map_err(|_| BytecodeError::Malformed("invalid utf-8 in constant pool"))?
                .to_string();
            ConstantPoolEntry::Utf8(s)
        }
        4 => ConstantPoolEntry::Class {
            name_index: r.read_u16()?,
        },
        5 => ConstantPoolEntry::FieldRef {
            class_index: r.read_u16()?,
            name_index: r.read_u16()?,
            type_index: r.read_u16()?,
        },
        6 => ConstantPoolEntry::MethodRef {
            class_index: r.read_u16()?,
            name_index: r.read_u16()?,
            descriptor_index: r.read_u16()?,
        },
        7 => ConstantPoolEntry::TypeDesc {
            desc_index: r.read_u16()?,
        },
        other => return Err(BytecodeError::UnknownConstantTag(other)),
    };
    Ok(entry)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn offset(&self) -> usize {
        self.pos
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], BytecodeError> {
        if self.pos + len > self.bytes.len() {
            return Err(BytecodeError::UnexpectedEof);
        }
        let slice = &self.bytes[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, BytecodeError> {
        Ok(self.read_bytes(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16, BytecodeError> {
        let b = self.read_bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, BytecodeError> {
        let b = self.read_bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_i64(&mut self) -> Result<i64, BytecodeError> {
        let b = self.read_bytes(8)?;
        Ok(i64::from_be_bytes(b.try_into().unwrap()))
    }

    fn read_f64(&mut self) -> Result<f64, BytecodeError> {
        let b = self.read_bytes(8)?;
        Ok(f64::from_be_bytes(b.try_into().unwrap()))
    }
}
