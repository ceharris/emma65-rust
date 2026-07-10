use std::fmt::{Display, Formatter};
use std::path::Path;

const IHEX_DATA: u8 = 0;
const IHEX_EOF: u8 = 1;
const IHEX_START: u8 = 5;

const SREC_HEADER: u8 = b'0';
const SREC_DATA: u8 = b'1';
const SREC_COUNT: u8 = b'5';
const SREC_START_ADDR: u8 = b'9';

/// An error that
/// occurs while loading a file into memory.
#[derive(Debug)]
pub enum LoadError {
    /// File format is not recognized.
    UnknownFormat(String),
    /// Invalid format within a recognized file type.
    Format(String),
    /// File/record extends beyond the bounds of the target memory space.
    OutOfBounds { address: u32, size: usize },
    /// Record checksum does not match the expected value
    ChecksumMismatch { address: u32, actual: u8, expected: u8 },
    /// Binary file size does not match the size of the target memory space.
    SizeMismatch { actual: usize, expected: usize },
    /// I/O error while loading a file.
    Io(std::io::Error),
}

impl Display for LoadError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::UnknownFormat(e) =>
                write!(f, "unknown format: {e}"),
            LoadError::Format(e) =>
                write!(f, "format error: {e}"),
            LoadError::OutOfBounds { address, size } =>
                write!(f, "out of bounds error at {address} size {size}"),
            LoadError::ChecksumMismatch { address, expected, actual } =>
                write!(f, "checksum mismatch error at {address} expected {expected} actual {actual}"),
            LoadError::SizeMismatch { actual, expected, } =>
                write!(f, "size mismatch; expected {expected} actual {actual}"),
            LoadError::Io(e) =>
                write!(f, "I/O error: {e}"),
        }
    }
}

/// Loads a memory segment of length N with the contents of a file.
///
/// # Arguments
/// * `path` - path to the subject file
/// * `mem` - target memory segment of length N
/// * `offset` - address at which the segment will be mapped
///
/// # Supported Formats
/// * Intel Hex; identified by filenames ending in `.hex`, `.ihx`, `.ihex`; supports records of
///   type 0 (Data), type 1 (End of File), and type 5 (Start Linear Address)
/// * Motorola S-Record; identified by filenames ending in `.s19`, `.srec`; supports records of
///   type 0 (Header), type 1 (Data), type 5 (Count), type 9 (Start Address, terminator)
/// * Binary: identified by filenames ending in `.bin`, `.rom`; size of the file must
///   exactly match the size of the target memory size
///
/// When loading Intel Hex or Motorola-S records, addresses specified by the records are biased by
/// the specified `offset`. The biased addressed and length of each record must be within the bounds
/// of the target memory address. Overlapping records are not detected.
//
pub async fn load_image(path: &Path, mem: &mut [u8], offset: usize)
        -> Result<Option<u16>, LoadError> {
    let data = tokio::fs::read(path).await.map_err(LoadError::Io)?;
    let extension = path.extension();
    if extension.is_none() {
        return Err(LoadError::UnknownFormat(
            "Cannot deduce format; hint: add an appropriate suffix to the filename".to_string()));
    }

    let extension = extension.unwrap();
    let suffix = extension.to_str();

    match suffix {
        Some("hex") | Some("ihx") | Some("ihex") => load_intel_hex(&data, mem, offset),
        Some("s19") | Some("srec") => load_motorola_srec(&data, mem, offset),
        Some("bin") | Some("rom") => load_binary(&data, mem),
        Some(_) => Err(LoadError::UnknownFormat(format!("Filename suffix '{}' not recognized", suffix.unwrap()))),
        None => Err(LoadError::UnknownFormat("Filename suffix cannot be decoded".to_string())),
    }
}

struct HexRecord<'a> {
    data: &'a[u8],
}

fn load_intel_hex(data: &[u8], mem: &mut [u8], offset: usize) -> Result<Option<u16>, LoadError> {
    let data = consume_preamble(data, b':');
    if data.is_empty() {
        return Ok(None);
    }
    let mut eof = false;
    let mut start_addr: Option<u16> = None;
    let mut record = HexRecord { data };
    while !eof {
        let mut checksum = Checksum::new();
        consume_sentinel(&mut record, b':')?;
        let rec_len = parse_hex_u8(&mut record)?;
        checksum.add_u8(rec_len);
        let addr = parse_hex_u16(&mut record)?;
        checksum.add_u16(addr);
        let rec_type = parse_hex_u8(&mut record)?;
        checksum.add_u8(rec_type);
        match rec_type {
            IHEX_DATA => {
                let index = addr as usize;
                if index < offset || index + (rec_len as usize) - offset > mem.len() {
                    return Err(LoadError::OutOfBounds { address: index as u32, size: rec_len as usize });
                }
                parse_data(&mut record, rec_len, addr, mem, offset, &mut checksum)?;
            },
            IHEX_EOF => {
                eof = true;
            },
            IHEX_START => {
                let upper_word = parse_hex_u16(&mut record)?;
                checksum.add_u16(upper_word);
                let lower_word = parse_hex_u16(&mut record)?;
                checksum.add_u16(lower_word);
                start_addr = Some(lower_word);
            },
            _ => return Err(LoadError::Format(format!("Unsupported record type {}", rec_type))),
        }
        let expected_ck = (!checksum.sum()).wrapping_add(1);
        let actual_ck = parse_hex_u8(&mut record)?;
        checksum.add_u8(actual_ck);
        if checksum.sum() != 0 {
            return Err(LoadError::ChecksumMismatch { address: addr as u32, expected: expected_ck, actual: actual_ck })
        }
        consume_to_next_record(&mut record)?;
    }
    Ok(start_addr)
}

fn load_motorola_srec(data: &[u8], mem: &mut [u8], offset: usize) -> Result<Option<u16>, LoadError> {
    let data = consume_preamble(data, b'S');
    if data.is_empty() {
        return Ok(None);
    }
    let mut eof = false;
    let mut start_addr: Option<u16> = None;
    let mut record = HexRecord { data };
    while !eof {
        let mut checksum = Checksum::new();
        let rec_type = parse_srec_type(&mut record)?;
        let rec_len = parse_hex_u8(&mut record)?;
        checksum.add_u8(rec_len);
        let addr = parse_hex_u16(&mut record)?;
        checksum.add_u16(addr);
        match rec_type {
            SREC_HEADER => {
                consume_data(&mut record, rec_len - 3, &mut checksum)?;
            }
            SREC_DATA => {
                let index = addr as usize;
                let data_len = rec_len as usize - 2 - 1;
                if index < offset || index + data_len - offset > mem.len() {
                    return Err(LoadError::OutOfBounds { address: index as u32, size: data_len });
                }
                parse_data(&mut record, rec_len - 3, addr, mem, offset, &mut checksum)?;
            }
            SREC_START_ADDR => {
                start_addr = Some(addr);
                eof = true;
            }
            SREC_COUNT => (),
            _ => return Err(LoadError::Format(format!("Unsupported record type '{}'", rec_type))),
        }
        let expected_ck = !checksum.sum();
        let actual_ck = parse_hex_u8(&mut record)?;
        if expected_ck != actual_ck {
            return Err(LoadError::ChecksumMismatch { address: addr as u32, expected: expected_ck, actual: actual_ck })
        }
        consume_to_next_record(&mut record)?;
    }
    Ok(start_addr)
}

fn load_binary(data: &[u8], mem: &mut [u8]) -> Result<Option<u16>, LoadError> {
    if data.len() == mem.len() {
        mem[0..data.len()].copy_from_slice(data);
        Ok(None)
    } else {
        Err(LoadError::SizeMismatch { actual: data.len(), expected: mem.len() })
    }
}

fn parse_data(record: &mut HexRecord, data_len: u8, addr: u16, mem: &mut[u8], offset: usize, checksum: &mut Checksum) -> Result<(), LoadError> {
    for i in 0..data_len {
        let b = parse_hex_u8(record)?;
        let index = addr as usize - offset + (i as usize);
        mem[index] = b;
        checksum.add_u8(b);
    }
    Ok(())
}

fn parse_hex_u16(record: &mut HexRecord) -> Result<u16, LoadError> {
    if record.data.len() >= 4 {
        let hi = parse_hex_u8(record)?;
        let lo = parse_hex_u8(record)?;
        Ok((hi as u16) << 8 | lo as u16)
    } else {
        Err(LoadError::Format("Expected hexadecimal digit, but reached end-of-file".to_string()))
    }
}

fn parse_hex_u8(record: &mut HexRecord) -> Result<u8, LoadError> {
    if record.data.len() >= 2 {
        let hi = parse_hex_digit(record.data[0])?;
        let lo = parse_hex_digit(record.data[1])?;
        record.data = &record.data[2..];
        Ok(hi << 4 | lo)
    } else {
        Err(LoadError::Format("Expected hexadecimal digit, but reached end-of-file".to_string()))
    }
}

fn parse_hex_digit(digit: u8) -> Result<u8, LoadError> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'A'..=b'F' => Ok(digit - b'A' + 10),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        _ => Err(LoadError::Format(format!("Expected hexadecimal digit, but got {digit}"))),
    }
}

/// Consumes preamble text leading up to the first occurrence of `sentinel` in the given
/// hex-formatted `data`.
fn consume_preamble(data: &[u8], sentinel: u8) -> &[u8] {
    let mut index: usize = 0;
    while index < data.len() && data[index] != sentinel {
        // skip text until newline reached
        while index < data.len() && data[index] != b'\n' {
            index += 1;
        }
        index += 1;  // skip the newline
        // skip trailing ASCII NUL
        while index < data.len() && data[index] == b'\0' {
            index += 1;
        }
    }
    &data[index..]
}

fn parse_srec_type(record: &mut HexRecord) -> Result<u8, LoadError> {
    consume_sentinel(record, b'S')?;
    if !record.data.is_empty() {
        let rec_type = record.data[0];
        record.data = &record.data[1..];
        Ok(rec_type)
    } else {
        Err(LoadError::Format("Expected record type, but got end-of-file".to_string()))
    }
}

fn consume_sentinel(record: &mut HexRecord, sentinel: u8) -> Result<(), LoadError> {
    if !record.data.is_empty() {
        if record.data[0] == sentinel {
            record.data = &record.data[1..];
            Ok(())
        } else {
            Err(LoadError::Format(format!("Expected sentinel '{}', but got '{}'", sentinel as char, record.data[0] as char)))
        }
    } else {
        Err(LoadError::Format(format!("Expected sentinel '{}', but got end-of-file", sentinel as char).to_string()))
    }
}

/// Consumes trailing whitespace and newline to reach the next expected
/// start-of-record sentinel.
fn consume_to_next_record(record: &mut HexRecord) -> Result<(), LoadError> {
    let mut index = 0;
    // find newline
    while index < record.data.len() && record.data[index] != b'\n' {
        let b = record.data[index];
        if b > b' ' {
            return Err(LoadError::Format(format!("Unexpected character '{}' following record ", b as char)));
        }
        index += 1;
    }
    if index < record.data.len() {
        index += 1;  // skip newline
        // skip trailing ASCII NULs
        while index < record.data.len() && record.data[index] == b'\0' {
            index += 1;
        }
        record.data = &record.data[index..];
    } else {
        record.data = &[];
    }
    Ok(())
}

fn consume_data(record: &mut HexRecord, data_len: u8, checksum: &mut Checksum) -> Result<(), LoadError> {
    for _ in 0..data_len {
        let b = parse_hex_u8(record)?;
        checksum.add_u8(b);
    }
    Ok(())
}

struct Checksum {
     sum: u8,
}

impl Checksum {

    fn new() -> Self {
        Checksum { sum: 0 }
    }

    fn sum(&self) -> u8 {
        self.sum
    }

    fn add_u8(&mut self, b: u8) {
        self.sum = self.sum.wrapping_add(b);
    }

    fn add_u16(&mut self, w: u16) {
        self.sum = self.sum.wrapping_add((w >> 8) as u8);
        self.sum = self.sum.wrapping_add((w & 0xff) as u8);
    }
}

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;
    use super::*;

    const IHEX_EXAMPLE: &str = "
:10010000214601360121470136007EFE09D2190140
:100110002146017E17C20001FF5F16002148011928
:10012000194E79234623965778239EDA3F01B2CAA7
:100130003F0156702B5E712B722B732146013421C7
:00000001FF
";

    const SREC_EXAMPLE: &str = "
S00F000068656C6C6F202020202000003C
S11F00007C0802A6900100049421FFF07C6C1B787C8C23783C6000003863000026
S11F001C4BFFFFE5398000007D83637880010014382100107C0803A64E800020E9
S111003848656C6C6F20776F726C642E0A0042
S5030003F9
S9030000FC
";

    const BIN_EXAMPLE: [u8; 8] = [0x00u8, 0xffu8, 0x55u8, 0xaau8, 0xdeu8, 0xadu8, 0xbeu8, 0xefu8];


    impl<'a> HexRecord<'a> {
        fn from(data: &'a str) -> Self {
            HexRecord {
                data: data.as_bytes()
            }
        }
    }

    #[test]
    fn load_intel_hex_success() {
        let hex_data = IHEX_EXAMPLE.as_bytes();
        let mut mem: [u8; 1024] = [0; 1024];
        let start_addr = load_intel_hex(hex_data, &mut mem[..], 0x100).unwrap();
        assert!(start_addr.is_none());
        assert_eq!(mem[0x0..0x4], vec![0x21u8, 0x46u8, 0x01u8, 0x36u8]);
        assert_eq!(mem[0x30..0x34], vec![0x3Fu8, 0x01u8, 0x56u8, 0x70u8]);
    }

    #[test]
    fn load_intel_hex_when_empty_data() {
        let hex_data: [u8; 0] = [];
        let mut mem: [u8; 0] = [];
        let start_addr = load_intel_hex(&hex_data, &mut mem, 0x0).unwrap();
        assert!(start_addr.is_none());
    }

    #[test]
    fn load_intel_hex_when_preamble_without_records() {
        let hex_data = "This is a test.\nThis is only a test\n".as_bytes();
        let mut mem: [u8; 0] = [];
        let start_addr = load_intel_hex(hex_data, &mut mem, 0x0).unwrap();
        assert!(start_addr.is_none());
    }

    #[test]
    fn load_intel_hex_when_no_data_records() {
        let hex_data = ":00000001FF".as_bytes();
        let mut mem: [u8; 0] = [];
        let start_addr = load_intel_hex(hex_data, &mut mem, 0x0).unwrap();
        assert!(start_addr.is_none());
    }

    #[test]
    fn load_intel_hex_when_unsupported_rec_type() {
        let hex_data = ":0000000907".as_bytes();
        let mut mem: [u8; 0] = [];
        let err = load_intel_hex(hex_data, &mut mem, 0x0).unwrap_err();
        assert!(matches!(err, LoadError::Format(message) if message.contains("record type")));
    }

    #[test]
    fn load_intel_hex_when_inconsistent_offset() {
        let hex_data = ":10010000214601360121470136007EFE09D2190140\n".as_bytes();
        let mut mem: [u8; 16] = [0; 16];
        let err = load_intel_hex(hex_data, &mut mem, 0).unwrap_err();
        assert!(matches!(err, LoadError::OutOfBounds { address: _, size: _ }));
    }

    #[test]
    fn load_intel_hex_when_no_eof_record() {
        let hex_data = ":10010000214601360121470136007EFE09D2190140\n".as_bytes();
        let mut mem: [u8; 16] = [0; 16];
        let err = load_intel_hex(hex_data, &mut mem, 0x100).unwrap_err();
        assert!(matches!(err, LoadError::Format(message) if message.contains("end-of-file")));
    }

    #[test]
    fn load_intel_hex_when_invalid_checksum() {
        let hex_data = ":00000001FE".as_bytes();
        let mut mem: [u8; 0] = [];
        let err = load_intel_hex(hex_data, &mut mem, 0x100).unwrap_err();
        assert!(matches!(err, LoadError::ChecksumMismatch { actual: a_ck, expected: e_ck, address: addr}
            if a_ck== 0xfeu8 && e_ck == 0xffu8 && addr == 0x0000));
    }

    #[test]
    fn load_intel_hex_with_start_record() {
        let hex_data = ":04000005000000CD2A\n:00000001FF\n".as_bytes();
        let mut mem: [u8; 0] = [];
        let start_addr = load_intel_hex(hex_data, &mut mem, 0x0).unwrap();
        assert!(matches!(start_addr, Some(addr) if addr == 0x00cd))
    }

    #[test]
    fn load_index_hex_can_write_to_end_of_segment() {
        // this record writes 6 bytes at 0xfffa (the 6502 reset vectors)
        let hex_data = ":06FFFA0000F000F000F031\n:00000001FF\n".as_bytes();
        let mut mem: [u8; 16] = [0; 16];
        let start_addr = load_intel_hex(hex_data, &mut mem, 0xfff0).unwrap();
        assert!(start_addr.is_none());
    }

    #[test]
    fn load_motorola_srec_success() {
        let hex_data = SREC_EXAMPLE.as_bytes();
        let mut mem: [u8; 1024] = [0; 1024];
        let start_addr = load_motorola_srec(hex_data, &mut mem[..], 0).unwrap();
        assert!(matches!(start_addr, Some(0)));
        assert_eq!(mem[0x0..0x4], vec![0x7Cu8, 0x08u8, 0x02u8, 0xA6u8]);
        assert_eq!(mem[0x38..0x3c], vec![0x48u8, 0x65u8, 0x6Cu8, 0x6Cu8]);
    }

    #[test]
    fn load_motorola_srec_when_empty_data() {
        let hex_data: [u8; 0] = [];
        let mut mem: [u8; 0] = [];
        let start_addr = load_motorola_srec(&hex_data, &mut mem, 0x0).unwrap();
        assert!(start_addr.is_none());
    }

    #[test]
    fn load_motorola_srec_when_preamble_without_records() {
        let hex_data = "This is a test.\nThis is only a test\n".as_bytes();
        let mut mem: [u8; 0] = [];
        let start_addr = load_motorola_srec(hex_data, &mut mem, 0x0).unwrap();
        assert!(start_addr.is_none());
    }

    #[test]
    fn load_motorola_srec_when_no_data_records() {
        let hex_data = "S9030000FC".as_bytes();
        let mut mem: [u8; 0] = [];
        let start_addr = load_motorola_srec(hex_data, &mut mem, 0x0).unwrap();
        assert!(matches!(start_addr, Some(0)));
    }

    #[test]
    fn load_motorola_srec_when_unsupported_rec_type() {
        let hex_data = "S70700000000FF".as_bytes();
        let mut mem: [u8; 0] = [];
        let err = load_motorola_srec(hex_data, &mut mem, 0x0).unwrap_err();
        assert!(matches!(err, LoadError::Format(message) if message.contains("record type")));
    }

    #[test]
    fn load_motorola_srec_when_inconsistent_offset() {
        let hex_data = "S11F00007C0802A6900100049421FFF07C6C1B787C8C23783C6000003863000026\n".as_bytes();
        let mut mem: [u8; 16] = [0; 16];
        let err = load_motorola_srec(hex_data, &mut mem, 0x100).unwrap_err();
        assert!(matches!(err, LoadError::OutOfBounds { address: _, size: _ }));
    }

    #[test]
    fn load_motorola_srec_can_write_to_end_of_segment() {
        // this record writes 6 bytes at 0xfffa (the 6502 reset vectors)
        let hex_data = "S109FFFA00F000F000F02D\nS9030000FC\n".as_bytes();
        let mut mem: [u8; 16] = [0; 16];
        let start_addr = load_motorola_srec(hex_data, &mut mem, 0xfff0).unwrap();
        assert!(matches!(start_addr, Some(0)));
    }

    #[test]
    fn load_motorola_srec_when_no_eof_record() {
        let hex_data = "S00F000068656C6C6F202020202000003C\n".as_bytes();
        let mut mem: [u8; 0] = [0; 0];
        let err = load_motorola_srec(hex_data, &mut mem, 0x100).unwrap_err();
        assert!(matches!(err, LoadError::Format(message) if message.contains("end-of-file")));
    }

    #[test]
    fn load_motorola_srec_when_invalid_checksum() {
        let hex_data = "S9030000FD".as_bytes();
        let mut mem: [u8; 0] = [];
        let err = load_motorola_srec(hex_data, &mut mem, 0x100).unwrap_err();
        assert!(matches!(err, LoadError::ChecksumMismatch { actual: a_ck, expected: e_ck, address: addr}
            if a_ck== 0xfdu8 && e_ck == 0xfcu8 && addr == 0x0000));
    }
    #[test]
    fn load_binary_success() {
        let bin_data: [u8; 8] = BIN_EXAMPLE;
        let mut mem: [u8; 8] = [0xff; 8];
        let start_addr = load_binary(&bin_data, &mut mem[..]).unwrap();
        assert!(start_addr.is_none());
        assert_eq!(mem[0x0..0x8], vec![0x00u8, 0xffu8, 0x55u8, 0xaau8, 0xdeu8, 0xadu8, 0xbeu8, 0xefu8]);
    }

    #[test]
    fn load_binary_when_size_mismatch() {
        let bin_data: [u8; 8] = [0x00u8, 0xffu8, 0x55u8, 0xaau8, 0xdeu8, 0xadu8, 0xbeu8, 0xefu8];
        let mut mem: [u8; 0] = [];
        let err = load_binary(&bin_data, &mut mem[..]).unwrap_err();
        assert!(matches!(err, LoadError::SizeMismatch { actual: a, expected: e} if a == 8 && e == 0));
    }

    #[test]
    fn parse_hex_digits() {
        assert_eq!(parse_hex_digit(b'0').unwrap(), 0u8);
        assert_eq!(parse_hex_digit(b'9').unwrap(), 9u8);
        assert_eq!(parse_hex_digit(b'A').unwrap(), 10u8);
        assert_eq!(parse_hex_digit(b'F').unwrap(), 15u8);
        assert_eq!(parse_hex_digit(b'a').unwrap(), 10u8);
        assert_eq!(parse_hex_digit(b'f').unwrap(), 15u8);
        let err = parse_hex_digit(b'!').unwrap_err();
        assert!(matches!(err, LoadError::Format(s) if s.contains("Expected")));
    }

    #[test]
    fn parse_hex_u8_values() {
        let mut record = HexRecord::from("00");
        assert!(matches!(parse_hex_u8(&mut record), Ok(v) if v == 0));
        let mut record = HexRecord::from("5A");
        assert!(matches!(parse_hex_u8(&mut record), Ok(v) if v == 0x5a));
        let mut record = HexRecord::from("FF");
        assert!(matches!(parse_hex_u8(&mut record), Ok(v) if v == 0xff));
        let mut record = HexRecord::from("55AA");
        let v1 = parse_hex_u8(&mut record).unwrap();
        let v2 = parse_hex_u8(&mut record).unwrap();
        assert_eq!(v1, 0x55);
        assert_eq!(v2, 0xaa);
        assert!(record.data.is_empty());
        let mut record = HexRecord::from("");
        let err = parse_hex_u8(&mut record).unwrap_err();
        assert!(matches!(err, LoadError::Format(s) if s.contains("end-of-file")))
    }

    #[test]
    fn parse_hex_u16_values() {
        let mut record = HexRecord::from("0000");
        assert!(matches!(parse_hex_u16(&mut record), Ok(v) if v == 0));
        let mut record = HexRecord::from("55AA");
        assert!(matches!(parse_hex_u16(&mut record), Ok(v) if v == 0x55aa));
        let mut record = HexRecord::from("FFFF");
        assert!(matches!(parse_hex_u16(&mut record), Ok(v) if v == 0xffff));
        let mut record = HexRecord::from("deadbeef");
        let v1 = parse_hex_u16(&mut record).unwrap();
        let v2 = parse_hex_u16(&mut record).unwrap();
        assert_eq!(v1, 0xdead);
        assert_eq!(v2, 0xbeef);
        assert!(record.data.is_empty());
        let mut record = HexRecord::from("");
        let err = parse_hex_u16(&mut record).unwrap_err();
        assert!(matches!(err, LoadError::Format(s) if s.contains("end-of-file")))
    }

    #[test]
    fn consume_preamble_when_no_preamble() {
        let data = vec![b':', b'\n'];
        let data = consume_preamble(&data, b':');
        assert_eq!(data.len(), 2);
        assert_eq!(data[0], b':');
    }

    #[test]
    fn consume_preamble_when_preamble_exists() {
        let data = "This is a test\n:\n".as_bytes();
        let data = consume_preamble(data, b':');
        assert_eq!(data.len(), 2);
        assert_eq!(data[0], b':');
    }

    #[test]
    fn consume_preamble_when_preamble_has_trailing_nuls() {
        let data = "This is a test\n\0\0:\n".as_bytes();
        let data = consume_preamble(data, b':');
        assert_eq!(data.len(), 2);
        assert_eq!(data[0], b':');
    }

    #[test]
    fn consume_sentinel_success() {
        let data = ":0";
        let mut hex_record = HexRecord::from(data);
        consume_sentinel(&mut hex_record, b':').unwrap();
        assert!(!hex_record.data.is_empty());
        assert_eq!(hex_record.data[0], b'0');
    }

    #[test]
    fn consume_sentinel_when_unexpected_char() {
        let data = "?0";
        let mut hex_record = HexRecord::from(data);
        let err = consume_sentinel(&mut hex_record, b':').unwrap_err();
        assert!(matches!(err, LoadError::Format(message) if message.contains("but got '")))
    }

    #[test]
    fn consume_sentinel_when_end_of_file() {
        let data = "";
        let mut hex_record = HexRecord::from(data);
        let err = consume_sentinel(&mut hex_record, b':').unwrap_err();
        assert!(matches!(err, LoadError::Format(message) if message.contains("end-of-file")))
    }

    #[test]
    fn consume_to_next_record_success() {
        let data = " \0\x1f\r\n\0\0:";
        let mut hex_record = HexRecord::from(data);
        consume_to_next_record(&mut hex_record).unwrap();
        assert!(!hex_record.data.is_empty());
        assert_eq!(hex_record.data[0], b':');
    }

    #[test]
    fn consume_to_next_record_when_trailing_text() {
        let data = "!";
        let mut hex_record = HexRecord::from(data);
        let err = consume_to_next_record(&mut hex_record).unwrap_err();
        assert!(matches!(err, LoadError::Format(message) if message.contains("Unexpected char")))
    }

    async fn validate_load_ihex(path: &NamedTempFile, data: &str) {
        tokio::fs::write(&path.path(), data.as_bytes()).await.unwrap();
        let mut mem: [u8; 1024] = [0; 1024];
        let start_addr = load_image(path.path(), &mut mem, 0x100).await.unwrap();
        assert!(start_addr.is_none());
    }

    #[tokio::test]
    async fn load_image_with_hex() {
        let path  = tempfile::Builder::new().suffix(".hex").tempfile().unwrap();
        validate_load_ihex(&path, IHEX_EXAMPLE).await;
    }

    #[tokio::test]
    async fn load_image_with_ihx() {
        let path  = tempfile::Builder::new().suffix(".ihx").tempfile().unwrap();
        validate_load_ihex(&path, IHEX_EXAMPLE).await;
    }

    #[tokio::test]
    async fn load_image_with_ihex() {
        let path  = tempfile::Builder::new().suffix(".ihex").tempfile().unwrap();
        validate_load_ihex(&path, IHEX_EXAMPLE).await;
    }

    async fn validate_load_srec(path: &NamedTempFile, data: &str) {
        tokio::fs::write(&path.path(), data.as_bytes()).await.unwrap();
        let mut mem: [u8; 1024] = [0; 1024];
        let start_addr = load_image(path.path(), &mut mem, 0).await.unwrap();
        assert!(matches!(start_addr, Some(0)));
    }

    #[tokio::test]
    async fn load_image_with_s19() {
        let path  = tempfile::Builder::new().suffix(".s19").tempfile().unwrap();
        validate_load_srec(&path, SREC_EXAMPLE).await;
    }

    #[tokio::test]
    async fn load_image_with_srec() {
        let path  = tempfile::Builder::new().suffix(".srec").tempfile().unwrap();
        validate_load_srec(&path, SREC_EXAMPLE).await;
    }

    async fn validate_load_bin(path: &NamedTempFile, data: &[u8]) {
        tokio::fs::write(&path.path(), data).await.unwrap();
        let mut mem: [u8; 8] = [0; 8];
        let start_addr = load_image(path.path(), &mut mem, 0).await.unwrap();
        assert!(start_addr.is_none());
    }

    #[tokio::test]
    async fn load_image_with_bin() {
        let path  = tempfile::Builder::new().suffix(".bin").tempfile().unwrap();
        validate_load_bin(&path, &BIN_EXAMPLE).await;
    }

    #[tokio::test]
    async fn load_image_with_rom() {
        let path  = tempfile::Builder::new().suffix(".rom").tempfile().unwrap();
        validate_load_bin(&path, &BIN_EXAMPLE).await;
    }

    #[tokio::test]
    async fn load_image_with_unrecognized_extension() {
        let path  = tempfile::Builder::new().suffix(".foo").tempfile().unwrap();
        let mut mem: [u8; 0] = [];
        let err = load_image(path.path(), &mut mem, 0).await.unwrap_err();
        assert!(matches!(err, LoadError::UnknownFormat(message) if message.contains("foo")));
    }

    #[tokio::test]
    async fn load_image_with_non_existent_file() {
        let path  = tempfile::Builder::new().suffix(".foo").tempfile().unwrap();
        tokio::fs::remove_file(&path).await.unwrap();
        let mut mem: [u8; 0] = [];
        let err = load_image(path.path(), &mut mem, 0).await.unwrap_err();
        assert!(matches!(err, LoadError::Io(_)));
    }

}