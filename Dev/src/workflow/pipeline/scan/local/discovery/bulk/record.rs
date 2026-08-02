//! Decoding one `getattrlistbulk` record.
//!
//! Every read here is bounds-checked and every field is taken in the order the kernel promises,
//! because the buffer is a packed byte stream whose layout depends on the attribute mask that was
//! requested. A misparse does not fail loudly — it yields a plausible-looking entry with the wrong
//! name, size or type, which a scan would then treat as fact.
//!
//! `ATTR_CMN_RETURNED_ATTRS` is what makes that safe: the kernel states which attributes it
//! actually returned, so a field that was not returned is skipped rather than read from whatever
//! follows.

use super::super::walk::{WalkEntry, WalkKind};
use crate::foundation::path::RootRelativePath;

pub(super) const BUFFER_SIZE: usize = 1024 * 1024;
pub(super) const ATTR_CMN_ERROR: u32 = 0x2000_0000;
pub(super) const VDIR: u32 = 2;
pub(super) const VLNK: u32 = 5;
pub(super) const SF_FIRMLINK: u32 = 0x0080_0000;
pub(super) const SF_DATALESS: u32 = 0x4000_0000;
pub(super) const DIR_MNTSTATUS_MNTPOINT: u32 = 0x0000_0001;

const REQUESTED_COMMON: u32 = libc::ATTR_CMN_RETURNED_ATTRS
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_MODTIME
    | libc::ATTR_CMN_ACCESSMASK
    | libc::ATTR_CMN_FLAGS
    | libc::ATTR_CMN_FILEID
    | ATTR_CMN_ERROR;

pub(super) static ATTRS: libc::attrlist = libc::attrlist {
    bitmapcount: libc::ATTR_BIT_MAP_COUNT,
    reserved: 0,
    commonattr: REQUESTED_COMMON,
    volattr: 0,
    dirattr: libc::ATTR_DIR_MOUNTSTATUS | libc::ATTR_DIR_DATALENGTH,
    fileattr: libc::ATTR_FILE_DATALENGTH,
    forkattr: 0,
};

#[derive(Debug)]
pub(super) struct ParsedEntry<'a> {
    pub(super) name: &'a [u8],
    pub(super) entry_error: Option<u32>,
    pub(super) dev: Option<u64>,
    pub(super) kind: Option<WalkKind>,
    pub(super) mtime_ms: Option<i64>,
    pub(super) mode: Option<u32>,
    pub(super) flags: Option<u32>,
    pub(super) file_id: Option<u64>,
    pub(super) mount_status: Option<u32>,
    pub(super) size: Option<u64>,
}

impl ParsedEntry<'_> {
    pub(super) fn complete(&self) -> bool {
        // Bulk metadata describes the covered directory/firmlink object, not the mounted/firmlink
        // root exposed through its descriptor. Force uncommon entries through descriptor-relative
        // metadata so mounted and firmlink roots keep their visible identity.
        self.entry_error.unwrap_or(0) == 0
            && self.dev.is_some()
            && self.kind.is_some()
            && self.mtime_ms.is_some()
            && self.mode.is_some()
            && self.flags.is_some()
            && self.file_id.is_some()
            && self.size.is_some()
            && match self.kind {
                Some(WalkKind::Dir) => matches!(
                    (self.mount_status, self.flags),
                    (Some(status), Some(flags))
                        if status & DIR_MNTSTATUS_MNTPOINT == 0
                            && flags & SF_FIRMLINK == 0
                ),
                Some(WalkKind::File | WalkKind::Symlink) => true,
                None => false,
            }
    }

    pub(super) fn into_walk_entry(self, relative: RootRelativePath) -> WalkEntry {
        let dev = self.dev.expect("complete bulk entry has a device id");
        let file_id = self.file_id.expect("complete bulk entry has a file id");
        WalkEntry {
            relative,
            kind: self.kind.expect("complete bulk entry has a kind"),
            size: self.size.expect("complete bulk entry has a size"),
            mtime_ms: self.mtime_ms.expect("complete bulk entry has an mtime"),
            file_id: Some(format!("{dev}:{file_id}")),
            mode: Some(self.mode.expect("complete bulk entry has a mode") & 0o7777),
            dataless: self.flags.expect("complete bulk entry has flags") & SF_DATALESS != 0,
        }
    }
}

pub(super) fn record_at(buffer: &[u8], offset: usize) -> Result<&[u8], &'static str> {
    let length_end = offset.checked_add(4).ok_or("record offset overflow")?;
    let length_bytes = buffer
        .get(offset..length_end)
        .ok_or("record length is truncated")?;
    let length = u32::from_ne_bytes(
        length_bytes
            .try_into()
            .map_err(|_| "invalid record length")?,
    ) as usize;
    if length < 24 {
        return Err("record is shorter than its length and returned-attribute fields");
    }
    let end = offset.checked_add(length).ok_or("record length overflow")?;
    buffer
        .get(offset..end)
        .ok_or("record extends beyond the returned buffer")
}

pub(super) fn parse_record(record: &[u8]) -> Result<ParsedEntry<'_>, &'static str> {
    let mut pos = 4usize;
    let returned = take(record, &mut pos, 20)?;
    let returned_common = read_u32(&returned[0..4])?;
    let returned_dir = read_u32(&returned[8..12])?;
    let returned_file = read_u32(&returned[12..16])?;
    if returned_common & !REQUESTED_COMMON != 0 {
        return Err("kernel returned an unrequested common attribute");
    }
    if returned_dir & !(libc::ATTR_DIR_MOUNTSTATUS | libc::ATTR_DIR_DATALENGTH) != 0 {
        return Err("kernel returned an unrequested directory attribute");
    }
    if returned_file & !libc::ATTR_FILE_DATALENGTH != 0 {
        return Err("kernel returned an unrequested file attribute");
    }

    let entry_error = if returned_common & ATTR_CMN_ERROR != 0 {
        Some(read_u32(take(record, &mut pos, 4)?)?)
    } else {
        None
    };

    let name = if returned_common & libc::ATTR_CMN_NAME != 0 {
        let reference_pos = pos;
        let reference = take(record, &mut pos, 8)?;
        let data_offset = i32::from_ne_bytes(
            reference[0..4]
                .try_into()
                .map_err(|_| "invalid name offset")?,
        );
        let data_length = read_u32(&reference[4..8])? as usize;
        if data_offset < 0 || data_length == 0 {
            return Err("invalid name reference");
        }
        let start = reference_pos
            .checked_add(data_offset as usize)
            .ok_or("name offset overflow")?;
        let end = start
            .checked_add(data_length)
            .ok_or("name length overflow")?;
        let name = record
            .get(start..end)
            .ok_or("name extends beyond its record")?;
        if name.last() != Some(&0) || name[..name.len() - 1].contains(&0) {
            return Err("name is not exactly one null-terminated string");
        }
        &name[..name.len() - 1]
    } else {
        return Err("required name attribute was not returned");
    };

    let dev = if returned_common & libc::ATTR_CMN_DEVID != 0 {
        let raw = read_i32(take(record, &mut pos, 4)?)?;
        Some(raw as u32 as u64)
    } else {
        None
    };
    let kind = if returned_common & libc::ATTR_CMN_OBJTYPE != 0 {
        Some(match read_u32(take(record, &mut pos, 4)?)? {
            VDIR => WalkKind::Dir,
            VLNK => WalkKind::Symlink,
            _ => WalkKind::File,
        })
    } else {
        None
    };
    let mtime_ms = if returned_common & libc::ATTR_CMN_MODTIME != 0 {
        let timespec = take(record, &mut pos, 16)?;
        Some(timespec_millis(
            read_i64(&timespec[0..8])?,
            read_i64(&timespec[8..16])?,
        ))
    } else {
        None
    };
    let mode = if returned_common & libc::ATTR_CMN_ACCESSMASK != 0 {
        Some(read_u32(take(record, &mut pos, 4)?)?)
    } else {
        None
    };
    let flags = if returned_common & libc::ATTR_CMN_FLAGS != 0 {
        Some(read_u32(take(record, &mut pos, 4)?)?)
    } else {
        None
    };
    let file_id = if returned_common & libc::ATTR_CMN_FILEID != 0 {
        Some(read_u64(take(record, &mut pos, 8)?)?)
    } else {
        None
    };
    let mount_status = if returned_dir & libc::ATTR_DIR_MOUNTSTATUS != 0 {
        Some(read_u32(take(record, &mut pos, 4)?)?)
    } else {
        None
    };
    let dir_size = if returned_dir & libc::ATTR_DIR_DATALENGTH != 0 {
        Some(read_u64(take(record, &mut pos, 8)?)?)
    } else {
        None
    };
    let file_size = if returned_file & libc::ATTR_FILE_DATALENGTH != 0 {
        Some(read_u64(take(record, &mut pos, 8)?)?)
    } else {
        None
    };

    Ok(ParsedEntry {
        name,
        entry_error,
        dev,
        kind,
        mtime_ms,
        mode,
        flags,
        file_id,
        mount_status,
        size: dir_size.or(file_size),
    })
}

pub(super) fn take<'a>(
    record: &'a [u8],
    pos: &mut usize,
    width: usize,
) -> Result<&'a [u8], &'static str> {
    let end = pos.checked_add(width).ok_or("attribute offset overflow")?;
    let field = record.get(*pos..end).ok_or("attribute is truncated")?;
    *pos = end;
    Ok(field)
}

pub(super) fn read_u32(bytes: &[u8]) -> Result<u32, &'static str> {
    Ok(u32::from_ne_bytes(
        bytes.try_into().map_err(|_| "invalid u32 attribute")?,
    ))
}

pub(super) fn read_i32(bytes: &[u8]) -> Result<i32, &'static str> {
    Ok(i32::from_ne_bytes(
        bytes.try_into().map_err(|_| "invalid i32 attribute")?,
    ))
}

pub(super) fn read_u64(bytes: &[u8]) -> Result<u64, &'static str> {
    Ok(u64::from_ne_bytes(
        bytes.try_into().map_err(|_| "invalid u64 attribute")?,
    ))
}

pub(super) fn read_i64(bytes: &[u8]) -> Result<i64, &'static str> {
    Ok(i64::from_ne_bytes(
        bytes.try_into().map_err(|_| "invalid i64 attribute")?,
    ))
}

pub(super) fn timespec_millis(seconds: i64, nanos: i64) -> i64 {
    if seconds < 0 || !(0..1_000_000_000).contains(&nanos) {
        return 0;
    }
    (seconds as i128 * 1000 + nanos as i128 / 1_000_000) as i64
}
