//! Constant pool — see nlvm-specs/docs/vm.md § Constant pool.

#[derive(Debug, Clone, PartialEq)]
pub enum ConstantPoolEntry {
    Int(i64),
    Float(f64),
    /// String literal or identifier (UTF-8 bytes).
    Utf8(String),
    /// Fully qualified class name (index to a `Utf8` entry).
    Class { name_index: u16 },
    FieldRef {
        class_index: u16,
        name_index: u16,
        type_index: u16,
    },
    MethodRef {
        class_index: u16,
        name_index: u16,
        descriptor_index: u16,
    },
    /// Type descriptor string (index to a `Utf8` entry).
    TypeDesc { desc_index: u16 },
}

impl ConstantPoolEntry {
    pub fn tag(&self) -> u8 {
        match self {
            ConstantPoolEntry::Int(_) => 1,
            ConstantPoolEntry::Float(_) => 2,
            ConstantPoolEntry::Utf8(_) => 3,
            ConstantPoolEntry::Class { .. } => 4,
            ConstantPoolEntry::FieldRef { .. } => 5,
            ConstantPoolEntry::MethodRef { .. } => 6,
            ConstantPoolEntry::TypeDesc { .. } => 7,
        }
    }
}

/// 1-indexed constant pool: index 0 is unused/reserved (see spec).
#[derive(Debug, Clone, Default)]
pub struct ConstantPool {
    entries: Vec<ConstantPoolEntry>,
}

impl ConstantPool {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn entries(&self) -> &[ConstantPoolEntry] {
        &self.entries
    }

    pub fn get(&self, index: u16) -> Option<&ConstantPoolEntry> {
        if index == 0 {
            return None;
        }
        self.entries.get(index as usize - 1)
    }

    fn push(&mut self, entry: ConstantPoolEntry) -> u16 {
        self.entries.push(entry);
        self.entries.len() as u16
    }

    /// Interns an entry, returning the existing index if an identical entry
    /// is already present (keeps modules compact and deterministic).
    fn intern(&mut self, entry: ConstantPoolEntry) -> u16 {
        if let Some(pos) = self.entries.iter().position(|e| e == &entry) {
            return (pos + 1) as u16;
        }
        self.push(entry)
    }

    pub fn add_int(&mut self, value: i64) -> u16 {
        self.intern(ConstantPoolEntry::Int(value))
    }

    pub fn add_float(&mut self, value: f64) -> u16 {
        // f64 has no total order/Eq; intern by bit pattern to keep dedup correct.
        if let Some(pos) = self.entries.iter().position(|e| {
            matches!(e, ConstantPoolEntry::Float(v) if v.to_bits() == value.to_bits())
        }) {
            return (pos + 1) as u16;
        }
        self.push(ConstantPoolEntry::Float(value))
    }

    pub fn add_utf8(&mut self, value: impl Into<String>) -> u16 {
        self.intern(ConstantPoolEntry::Utf8(value.into()))
    }

    pub fn add_class(&mut self, fully_qualified_name: &str) -> u16 {
        let name_index = self.add_utf8(fully_qualified_name);
        self.intern(ConstantPoolEntry::Class { name_index })
    }

    pub fn add_field_ref(&mut self, class_index: u16, name_index: u16, type_index: u16) -> u16 {
        self.intern(ConstantPoolEntry::FieldRef {
            class_index,
            name_index,
            type_index,
        })
    }

    pub fn add_method_ref(&mut self, class_index: u16, name_index: u16, descriptor_index: u16) -> u16 {
        self.intern(ConstantPoolEntry::MethodRef {
            class_index,
            name_index,
            descriptor_index,
        })
    }

    pub fn add_type_desc(&mut self, descriptor: &str) -> u16 {
        let desc_index = self.add_utf8(descriptor);
        self.intern(ConstantPoolEntry::TypeDesc { desc_index })
    }

    /// Resolves a `Utf8` entry's string value at `index`.
    pub fn utf8_at(&self, index: u16) -> Option<&str> {
        match self.get(index) {
            Some(ConstantPoolEntry::Utf8(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn class_name_at(&self, index: u16) -> Option<&str> {
        match self.get(index) {
            Some(ConstantPoolEntry::Class { name_index }) => self.utf8_at(*name_index),
            _ => None,
        }
    }

    pub fn type_desc_at(&self, index: u16) -> Option<&str> {
        match self.get(index) {
            Some(ConstantPoolEntry::TypeDesc { desc_index }) => self.utf8_at(*desc_index),
            _ => None,
        }
    }
}
