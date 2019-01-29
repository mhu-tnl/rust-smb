// smbc is library wrapping libsmbclient from Samba project
// Copyright (c) 2016 Konstantin Gribov
//
// This file is part of smbc.
//
// smbc is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// smbc is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with smbc. If not, see <http://www.gnu.org/licenses/>.

//! `smbc` is wrapper library around `libsmbclient` from Samba project.

use std::ffi::{CStr, CString};
use std::fmt;
use std::io;
use std::io::{Error, ErrorKind, Read, Seek, SeekFrom, Write};
use std::mem::zeroed;
use std::os::raw::c_void;
use std::os::unix::ffi::OsStrExt;
use std::panic;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex};

use crate::parser::*;
use crate::result::{Error as SmbcError, Result};
use crate::util::*;
use chrono::*;
use libc::{c_char, c_int, mode_t, off_t, strncpy, EINVAL};
use nix::fcntl::OFlag;
use nix::sys::stat::Mode;
use nom::types::CompleteByteSlice;
use smbclient_sys::*;

use bitflags::bitflags;
use lazy_static::*;
use log::{error, trace};
use percent_encoding::*;

lazy_static! {
    pub static ref user_data: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![
        "WORKGROUP".to_string(),
        "guest".to_string(),
        "".to_string()
    ]));
}

#[derive(Clone)]
/// a pointer to hold the smbc context
struct SmbcPtr(*mut SMBCCTX);
unsafe impl Send for SmbcPtr {}
unsafe impl Sync for SmbcPtr {}
impl Drop for SmbcPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            trace!(target: "smbc", "closing smbcontext");
            unsafe {
                smbc_free_context(self.0, 1 as c_int);
            }
        }
    }
}

#[derive(Clone)]
/// The Smbc Object.  Contains a pointer to a Samba context
pub struct Smbc {
    context: Arc<Mutex<SmbcPtr>>,
    pub chmod_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, fname: *const c_char, mode: mode_t) -> c_int),
    pub close_fn: (unsafe extern "C" fn(c: *mut SMBCCTX, file: *mut SMBCFILE) -> c_int),
    pub closedir_fn: (unsafe extern "C" fn(c: *mut SMBCCTX, dir: *mut SMBCFILE) -> c_int),
    pub creat_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, path: *const c_char, mode: mode_t) -> *mut SMBCFILE),
    pub fstat_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, file: *mut SMBCFILE, st: *mut stat) -> c_int),
    pub fstatvfs_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, file: *mut SMBCFILE, st: *mut statvfs) -> c_int),
    pub fstatdir_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, dir: *mut SMBCFILE, st: *mut stat) -> c_int),
    pub ftruncate_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, f: *mut SMBCFILE, size: off_t) -> c_int),
    pub getdents_fn: (unsafe extern "C" fn(
        c: *mut SMBCCTX,
        dir: *mut SMBCFILE,
        dirp: *mut smbc_dirent,
        count: c_int,
    ) -> c_int),
    pub getxattr_fn: (unsafe extern "C" fn(
        c: *mut SMBCCTX,
        fname: *const c_char,
        name: *const c_char,
        value: *const c_void,
        size: usize,
    ) -> c_int),
    pub listxattr_fn: (unsafe extern "C" fn(
        context: *mut SMBCCTX,
        fname: *const c_char,
        list: *mut c_char,
        size: usize,
    ) -> c_int),
    pub lseek_fn: (unsafe extern "C" fn(
        c: *mut SMBCCTX,
        file: *mut SMBCFILE,
        offset: off_t,
        whence: c_int,
    ) -> off_t),
    pub lseekdir_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, dir: *mut SMBCFILE, offset: off_t) -> c_int),
    pub mkdir_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, fname: *const c_char, mode: mode_t) -> c_int),
    pub open_fn: (unsafe extern "C" fn(
        c: *mut SMBCCTX,
        fname: *const c_char,
        flags: c_int,
        mode: mode_t,
    ) -> *mut SMBCFILE),
    pub opendir_fn: (unsafe extern "C" fn(c: *mut SMBCCTX, fname: *const c_char) -> *mut SMBCFILE),
    pub read_fn: (unsafe extern "C" fn(
        c: *mut SMBCCTX,
        file: *mut SMBCFILE,
        buf: *mut c_void,
        count: usize,
    ) -> isize),
    pub readdir_fn: (unsafe extern "C" fn(c: *mut SMBCCTX, dir: *mut SMBCFILE) -> *mut smbc_dirent),
    pub removexattr_fn: (unsafe extern "C" fn(
        context: *mut SMBCCTX,
        fname: *const c_char,
        name: *const c_char,
    ) -> c_int),
    pub rename_fn: (unsafe extern "C" fn(
        ocontext: *mut SMBCCTX,
        oname: *const c_char,
        ncontext: *mut SMBCCTX,
        nname: *const c_char,
    ) -> c_int),
    pub rmdir_fn: (unsafe extern "C" fn(c: *mut SMBCCTX, fname: *const c_char) -> c_int),
    pub setxattr_fn: (unsafe extern "C" fn(
        context: *mut SMBCCTX,
        fname: *const c_char,
        name: *const c_char,
        value: *const c_void,
        size: usize,
        flags: c_int,
    ) -> c_int),
    pub stat_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, fname: *const c_char, st: *mut stat) -> c_int),
    pub statvfs_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, path: *mut c_char, st: *mut statvfs) -> c_int),
    pub telldir_fn: (unsafe extern "C" fn(c: *mut SMBCCTX, dir: *mut SMBCFILE) -> off_t),
    pub unlink_fn: (unsafe extern "C" fn(c: *mut SMBCCTX, fname: *const c_char) -> c_int),
    pub utimes_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, fname: *const c_char, tbuf: *mut timeval) -> c_int),
    pub write_fn: (unsafe extern "C" fn(
        c: *mut SMBCCTX,
        file: *mut SMBCFILE,
        buf: *const c_void,
        count: usize,
    ) -> isize),
}

bitflags! {
    /// Te Attribute Flags needed in a setxattr call
    pub struct XAttrFlags :i32 {
        /// zeroed
        const SMBC_XATTR_FLAG_NONE = 0x0;
        /// create new attribute
        const SMBC_XATTR_FLAG_CREATE = 0x1;
        /// replace attribute
        const SMBC_XATTR_FLAG_REPLACE = 0x2;
    }
}

bitflags! {
    /// ACL attribute mask constants
    pub struct XAttrMask : i32 {
        /// Allow Read Access
        const R = 0x0012_0089;
        /// Allow Write Access
        const W = 0x0012_0116;
        /// Execute permission on the object
        const X = 0x0012_00a0;
        /// Delete the object
        const D = 0x0001_0000;
        /// Change permissions
        const P = 0x0004_0000;
        /// Take ownership
        const O = 0x0008_0000;
        /// None
        const N = 0x0000_0000;
        /// Equivalent to 'RX' permissions
        const READ = 0x0012_00a9;
        /// Equivalent to RXWD permissions
        const CHANGE = 0x0013_01bf;
        /// Equivalent to RWXDPO permissions
        const FULL = 0x001f_01ff;
    }
}

impl XAttrMask {
    pub fn from_string(mask: &str) -> Self {
        let mut m = XAttrMask::N;
        if mask == "FULL" {
            return XAttrMask::FULL;
        }
        if mask == "CHANGE" {
            return XAttrMask::CHANGE;
        }
        if mask == "READ" {
            return XAttrMask::READ;
        }
        if mask.contains('O') {
            m |= XAttrMask::O;
        }
        if mask.contains('P') {
            m |= XAttrMask::P;
        }
        if mask.contains('D') {
            m |= XAttrMask::D;
        }
        if mask.contains('X') {
            m |= XAttrMask::X;
        }
        if mask.contains('W') {
            m |= XAttrMask::W;
        }
        if mask.contains('R') {
            m |= XAttrMask::R;
        }
        m
    }
}

impl fmt::Display for XAttrMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buff = String::new();
        if self.contains(XAttrMask::FULL) {
            buff.push_str("FULL");
            return write!(f, "{}", buff);
        }
        if self.contains(XAttrMask::CHANGE) {
            buff.push_str("CHANGE");
            return write!(f, "{}", buff);
        }
        if self.contains(XAttrMask::READ) {
            buff.push_str("READ");
            return write!(f, "{}", buff);
        }
        if self.contains(XAttrMask::R) {
            buff.push_str("R");
        }
        if self.contains(XAttrMask::W) {
            buff.push_str("W");
        }
        if self.contains(XAttrMask::X) {
            buff.push_str("X");
        }
        if self.contains(XAttrMask::D) {
            buff.push_str("D");
        }
        if self.contains(XAttrMask::P) {
            buff.push_str("P");
        }
        if self.contains(XAttrMask::O) {
            buff.push_str("O");
        }
        if self.contains(XAttrMask::N) && buff.is_empty() {
            buff.push_str("N");
        }
        write!(f, "{}", buff)
    }
}

bitflags! {
    /// Dos Mode constants
    pub struct DosMode : i32 {
        /// Readonly file.  Note the read-only attribute is not honored on directories
        const READONLY = 0x01;
        /// file/dir is not included in an ordinary dir listing
        const HIDDEN = 0x02;
        /// OS use
        const SYSTEM = 0x04;
        const VOLUME_ID = 0x08;
        /// identifies a directory
        const DIRECTORY = 0x10;
        /// An archive file or directory. Applications typically use this attribute to mark files for backup or removal.
        const ARCHIVE = 0x20;
        /// reserved for system use
        const DEVICE = 0x40;
        /// valid only by itself. No other attributes are set.
        const NORMAL = 0x80;
        /// file used for temp storage.
        const TEMPORARY = 0x100;
        /// sparse file
        const SPARSE_FILE = 0x200;
        /// has sym link
        const REPARSE_POINT = 0x400;
        /// a file or directory that is compressed. In a file, all data is compressed.
        const COMPRESSED = 0x800;
        /// data moved offline storage
        const OFFLINE = 0x1000;
        /// file not to be indexed with context indexing service
        const NOT_CONTENT_INDEXED = 0x2000;
        /// Encrypted file/dir
        const ENCRYPTED = 0x4000;
        /// dir or data stream conf with integrity (ReFS vol only)
        const INTEGRITY_STREAM = 0x8000;
    }
}

#[derive(Debug, Clone)]
/// File Type
pub enum SmbcType {
    WORKGROUP = 1,
    SERVER = 2,
    FILESHARE = 3,
    PRINTERSHARE = 4,
    COMMSSHARE = 5,
    IPCSHARE = 6,
    DIR = 7,
    FILE = 8,
    LINK = 9,
}

impl SmbcType {
    fn from(t: u32) -> io::Result<SmbcType> {
        match t {
            1 => Ok(SmbcType::WORKGROUP),
            2 => Ok(SmbcType::SERVER),
            3 => Ok(SmbcType::FILESHARE),
            4 => Ok(SmbcType::PRINTERSHARE),
            5 => Ok(SmbcType::COMMSSHARE),
            6 => Ok(SmbcType::IPCSHARE),
            7 => Ok(SmbcType::DIR),
            8 => Ok(SmbcType::FILE),
            9 => Ok(SmbcType::LINK),
            _ => Err(io::Error::new(
                ErrorKind::InvalidData,
                "Unknown file type: ",
            )),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
///
/// Samba XAttribute names are formatted as such:
/// system.*
/// system.*+
/// system.nt_sec_desc.*
/// system.nt_sec_desc.*+
/// system.nt_sec_desc.revision
/// system.nt_sec_desc.owner
/// system.nt_sec_desc.owner+
/// system.nt_sec_desc.group
/// system.nt_sec_desc.group+
/// system.nt_sec_desc.acl.*
/// system.nt_sec_desc.acl.*+
/// system.nt_sec_desc.acl<SID>
/// system.nt_sec_desc.acl+<SID>
/// system.dos_attr.*
/// system.dos_attr.mode
/// system.dos_attr.inode
/// system.dos_attr.size
/// system.dos_attr.atime
/// system.dos_attr.mtime
/// system.dos_attr.ctime
///
pub enum SmbcXAttr {
    /// system.*
    All,
    /// system.*+
    AllPlus,
    /// Get xattr only (includes attribute exclusion)
    AllExclude(Vec<SmbcExclude>),
    /// Get xattr only (includes attribute exclusion)
    AllExcludePlus(Vec<SmbcExclude>),
    /// Dos attributes
    DosAttr(SmbcDosAttr),
    /// NT Security Description Attributes
    AclAttr(SmbcAclAttr),
}

impl fmt::Display for SmbcXAttr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmbcXAttr::All => write!(f, "system.*"),
            SmbcXAttr::AllPlus => write!(f, "system.*+"),
            SmbcXAttr::AllExclude(s) => write!(f, "system.*!{}", separated(s, "!")),
            SmbcXAttr::AllExcludePlus(s) => write!(f, "system.*+!{}", separated(s, "!")),
            SmbcXAttr::DosAttr(d) => d.fmt(f),
            SmbcXAttr::AclAttr(a) => a.fmt(f),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// commands for dos attributes
pub enum SmbcDosAttr {
    /// system.dos_attr.*
    All,
    /// Get xattr only
    AllExclude(Vec<SmbcExclude>),
    /// system.dos_attr.atime
    Atime,
    /// system.dos_attr.ctime
    Ctime,
    /// system.dos_attr.mode
    Mode,
    /// system.dos_attr.mtime
    Mtime,
    /// system.dos_attr.inode
    Inode,
    /// system.dos_attr.size
    Size,
}

impl fmt::Display for SmbcDosAttr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmbcDosAttr::All => write!(f, "system.dos_attr.*"),
            SmbcDosAttr::AllExclude(s) => write!(f, "system.dos_attr.*!{}", separated(s, "!")),
            SmbcDosAttr::Atime => write!(f, "system.dos_attr.a_time"),
            SmbcDosAttr::Ctime => write!(f, "system.dos_attr.c_time"),
            SmbcDosAttr::Mode => write!(f, "system.dos_attr.mode"),
            SmbcDosAttr::Mtime => write!(f, "system.dos_attr.m_time"),
            SmbcDosAttr::Inode => write!(f, "system.dos_attr.inode"),
            SmbcDosAttr::Size => write!(f, "system.dos_attr.size"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// commands for NT Security Descriptor
pub enum SmbcAclAttr {
    /// remove use only (specific format) numeric Acl
    Acl(ACE),
    /// remove use only (specific format) named Acl
    AclPlus(ACE),
    /// system.nt_sec_desc.acl.*
    AclAll,
    /// system.nt_sec_desc.acl.*+
    AclAllPlus,
    /// set use only specific numeric Acl
    AclNone,
    /// set use only specific named Acl
    AclNonePlus,
    /// get use only specific numeric Acl
    AclSid(Sid),
    /// get use only specific named Acl
    AclSidPlus(Sid),
    /// system.nt_sec_desc.*
    All,
    /// system.nt_sec_desc.*+
    AllPlus,
    /// get use only, can exclude attributes
    AllExclude(Vec<SmbcExclude>),
    /// get use only, can exclude attributes
    AllExcludePlus(Vec<SmbcExclude>),
    /// system.nt_sec_desc.group
    Group,
    /// system.nt_sec_desc.group+
    GroupPlus,
    /// system.nt_sec_desc.revision
    Revision,
    /// system.nt_sec_desc.owner
    Owner,
    /// system.nt_sec_desc.owner+
    OwnerPlus,
}

impl fmt::Display for SmbcAclAttr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmbcAclAttr::Acl(s) => write!(f, "system.nt_sec_desc.acl{}", format!(":{}", s)),
            SmbcAclAttr::AclAll => write!(f, "system.nt_sec_desc.acl.*"),
            SmbcAclAttr::AclAllPlus => write!(f, "system.nt_sec_desc.acl.*+"),
            SmbcAclAttr::AclNone => write!(f, "system.nt_sec_desc.acl"),
            SmbcAclAttr::AclNonePlus => write!(f, "system.nt_sec_desc.acl+"),
            SmbcAclAttr::AclPlus(s) => write!(f, "system.nt_sec_desc.acl+{}", format!(":{}", s)),
            SmbcAclAttr::AclSid(s) => write!(f, "system.nt_sec_desc.acl{}", format!("{}", s)),
            SmbcAclAttr::AclSidPlus(s) => write!(f, "system.nt_sec_desc.acl+{}", format!("{}", s)),
            SmbcAclAttr::All => write!(f, "system.nt_sec_desc.*"),
            SmbcAclAttr::AllPlus => write!(f, "system.nt_sec_desc.*+"),
            SmbcAclAttr::AllExclude(s) => write!(f, "system.nt_sec_desc.*!{}", separated(s, "!")),
            SmbcAclAttr::AllExcludePlus(s) => {
                write!(f, "system.nt_sec_desc.*!{}", separated(s, "!"))
            }
            SmbcAclAttr::Group => write!(f, "system.nt_sec_desc.group"),
            SmbcAclAttr::GroupPlus => write!(f, "system.nt_sec_desc.group+"),
            SmbcAclAttr::Revision => write!(f, "system.nt_sec_desc.revision"),
            SmbcAclAttr::Owner => write!(f, "system.nt_sec_desc.owner"),
            SmbcAclAttr::OwnerPlus => write!(f, "system.nt_sec_desc.owner+"),
        }
    }
}

///
/// Values for input change values (for .* calls)
/// REVISION:{}
/// OWNER:{}
/// OWNER+:{}
/// GROUP:{}
/// GROUP+:{}
/// ACL:{}
/// ACL+:{}
///
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SmbcAclValue {
    /// ACL:{}, numeric ace
    Acl(ACE),
    /// ACL+:{}, named acl
    AclPlus(ACE),
    /// Group:{}, numeric sid
    Group(Sid),
    /// GROUP+:{}, named sid
    GroupPlus(String),
    /// OWNER:{}, numeric sid
    Owner(Sid),
    /// OWNER+:{}, named sid
    OwnerPlus(String),
    /// REVISION:{}, revision number (NOTE: 1 is the only safe revision number...)
    Revision(u64),
}

impl fmt::Display for SmbcAclValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmbcAclValue::Acl(s) => write!(f, "ACL:{}", format!("{}", s)),
            SmbcAclValue::AclPlus(s) => write!(f, "ACL+:{}", format!("{}", s)),
            SmbcAclValue::Group(s) => write!(f, "GROUP:{}", format!("{}", s)),
            SmbcAclValue::GroupPlus(s) => write!(f, "GROUP+:{}", s),
            SmbcAclValue::Revision(i) => write!(f, "REVISION:{}", i),
            SmbcAclValue::Owner(s) => write!(f, "OWNER:{}", format!("{}", s)),
            SmbcAclValue::OwnerPlus(s) => write!(f, "OWNER+:{}", s),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// The type of an ACE can be either Allowed or Denied to allow/deny access to the SID
pub enum AceAtype {
    /// Allow access to the SID
    ALLOWED,
    /// Deny Access to the SID
    DENIED,
}

bitflags! {
    /// Note: currently these flags can only be specified as decimal or hex values.
    /// 9 or 2 is usually the value for directories
    pub struct AceFlag : i32{
        /// This is usually the flag for files
        const NONE = 0;
        /// Non-container child objects inherit the ACE as an effective ACE
        const SEC_ACE_FLAG_OBJECT_INHERIT = 0x1;
        /// The ACE has an effect on child namespaces as well as the current namespace
        const SEC_ACE_FLAG_CONTAINER_INHERIT = 0x2;
        /// The ACE applies only to the current namespace and immediate children
        const SEC_ACE_FLAG_NO_PROPAGATE_INHERIT = 0x4;
        /// The ACE applies only to child namespaces
        const SEC_ACE_FLAG_INHERIT_ONLY = 0x8;
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Sid(pub Vec<u64>);
impl fmt::Display for Sid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut dash_separated = String::new();
        dash_separated.push_str("S-1-");
        let s = separated(&self.0, "-");
        dash_separated.push_str(&s);
        write!(f, "{}", dash_separated)
    }
}

///
/// Used for parsing individual ACL:SID output from getxattr
///
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SidType {
    /// A numeric Sid
    Numeric(Option<Sid>),
    /// A named Sid
    Named(Option<String>),
}
impl fmt::Display for SidType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SidType::Numeric(Some(s)) => s.fmt(f),
            SidType::Numeric(None) => write!(f, ""),
            SidType::Named(Some(s)) => write!(f, "{}", s),
            SidType::Named(None) => write!(f, ""),
        }
    }
}

///
/// Please NOTE that we use SidType for holding the Sid Value
/// Which means the SID might be NONE (Please note that this should
/// ONLY be the case when parsing individual ACL:SID values from getxattr, in
/// which case you should manually set the SID.  Also, individual parsers will
/// always return NUMERIC ACE)
///
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ACE {
    /// A Numeric ACE
    Numeric(SidType, AceAtype, AceFlag, XAttrMask),
    /// A Named ACE
    /// Please note that the String input for xattribute mask only works on the
    /// inputs FULL, CHANGE, and READ
    Named(SidType, AceAtype, AceFlag, String),
}

impl ACE {
    pub fn new_num(sid: Sid, atype: AceAtype, flag: AceFlag, mask: XAttrMask) -> Self {
        ACE::Numeric(SidType::Numeric(Some(sid)), atype, flag, mask)
    }

    pub fn new_named_with_mask(sid: &str, atype: AceAtype, flag: AceFlag, mask: XAttrMask) -> Self {
        ACE::Named(
            SidType::Named(Some(sid.to_string())),
            atype,
            flag,
            mask.to_string(),
        )
    }
    pub fn new_named(sid: &str, atype: AceAtype, flag: AceFlag, mask: &str) -> Self {
        ACE::Named(
            SidType::Named(Some(sid.to_string())),
            atype,
            flag,
            mask.to_string(),
        )
    }

    pub fn sid_string(&self) -> Result<String> {
        match self {
            ACE::Numeric(SidType::Numeric(Some(sid)), _, _, _) => Ok(sid.to_string()),
            ACE::Named(SidType::Named(Some(sid)), _, _, _) => Ok(sid.to_string()),
            ACE::Numeric(SidType::Numeric(None), _, _, _) => {
                error!("SidType should not be None!");
                Err(SmbcError::SmbcXAttrError("SidType is None!".to_string()))
            }
            ACE::Named(SidType::Named(None), _, _, _) => {
                error!("SidType should not be None!");
                Err(SmbcError::SmbcXAttrError("SidType is None!".to_string()))
            }
            _ => Err(SmbcError::SmbcXAttrError(
                "Mismatched ACE and SidType!".to_string(),
            )),
        }
    }

    pub fn sid(&self) -> Result<Sid> {
        match self {
            ACE::Numeric(SidType::Numeric(Some(sid)), _, _, _) => Ok(sid.clone()),
            ACE::Named(SidType::Named(Some(sid)), _, _, _) => {
                match sid_parse(CompleteByteSlice(sid.as_bytes())) {
                    Ok((_, parse_sid)) => Ok(parse_sid),
                    Err(_e) => Err(SmbcError::SmbcXAttrError(
                        "Unable to parse SID!".to_string(),
                    )),
                }
            }
            ACE::Numeric(SidType::Numeric(None), _, _, _) => {
                error!("SidType should not be None!");
                Err(SmbcError::SmbcXAttrError("SidType is None!".to_string()))
            }
            ACE::Named(SidType::Named(None), _, _, _) => {
                error!("SidType should not be None!");
                Err(SmbcError::SmbcXAttrError("SidType is None!".to_string()))
            }
            _ => Err(SmbcError::SmbcXAttrError(
                "Mismatched ACE and SidType!".to_string(),
            )),
        }
    }

    pub fn acetype(&self) -> Result<AceAtype> {
        match self {
            ACE::Numeric(SidType::Numeric(_), atype, _, _) => Ok(atype.clone()),
            ACE::Named(SidType::Named(_), atype, _, _) => Ok(atype.clone()),
            _ => Err(SmbcError::SmbcXAttrError(
                "Mismatched ACE and SidType!".to_string(),
            )),
        }
    }

    pub fn aceflag(&self) -> Result<AceFlag> {
        match self {
            ACE::Numeric(SidType::Numeric(_), _, flag, _) => Ok(*flag),
            ACE::Named(SidType::Named(_), _, flag, _) => Ok(*flag),
            _ => Err(SmbcError::SmbcXAttrError(
                "Mismatched ACE and SidType!".to_string(),
            )),
        }
    }

    pub fn mask_string(&self) -> Result<String> {
        match self {
            ACE::Numeric(SidType::Numeric(_), _, _, mask) => Ok(mask.to_string()),
            ACE::Named(SidType::Named(_), _, _, mask) => Ok(mask.clone()),
            _ => Err(SmbcError::SmbcXAttrError(
                "Mismatched ACE and SidType!".to_string(),
            )),
        }
    }

    pub fn mask(&self) -> Result<XAttrMask> {
        match self {
            ACE::Numeric(SidType::Numeric(_), _, _, mask) => Ok(*mask),
            ACE::Named(SidType::Named(_), _, _, mask) => Ok(XAttrMask::from_string(mask)),
            _ => Err(SmbcError::SmbcXAttrError(
                "Mismatched ACE and SidType!".to_string(),
            )),
        }
    }

    pub fn is_numeric(&self) -> Result<bool> {
        match self {
            ACE::Numeric(SidType::Numeric(_), _, _, _) => Ok(true),
            ACE::Named(SidType::Named(_), _, _, _) => Ok(false),
            _ => Err(SmbcError::SmbcXAttrError(
                "Mismatched ACE and SidType!".to_string(),
            )),
        }
    }
}

impl fmt::Display for ACE {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ACE::Numeric(s, atype, flags, mask) => match atype {
                AceAtype::ALLOWED => {
                    write!(f, "{}:0/{}/{}", format!("{}", s), flags.bits(), mask.bits(),)
                }
                AceAtype::DENIED => {
                    write!(f, "{}:1/{}/{}", format!("{}", s), flags.bits(), mask.bits())
                }
            },
            ACE::Named(sid, atype, flags, mask) => match atype {
                AceAtype::ALLOWED => write!(
                    f,
                    "{}:ALLOWED/{:x}/{}",
                    format!("{}", sid),
                    flags.bits(),
                    mask
                ),
                AceAtype::DENIED => write!(
                    f,
                    "{}:DENIED/{:x}/{}",
                    format!("{}", sid),
                    flags.bits(),
                    mask
                ),
            },
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// PLEASE Note that INode and Size values are ignored in setxattr calls
/// INode and Size are used PURELY for parsing .* calls (Since there is
/// no point in using them to change xattr values)
pub enum SmbcDosValue {
    /// Mode value
    MODE(DosMode),
    /// Atime value
    ATime(u64),
    /// Ctime value
    CTime(u64),
    /// MTime value
    MTime(u64),
    /// INode value
    INode(u64),
    /// Size value
    Size(i64),
}

impl fmt::Display for SmbcDosValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmbcDosValue::MODE(m) => write!(f, "MODE:{}", m.bits()),
            SmbcDosValue::ATime(m) => write!(f, "A_TIME:{}", m),
            SmbcDosValue::CTime(m) => write!(f, "C_TIME:{}", m),
            SmbcDosValue::MTime(m) => write!(f, "M_TIME:{}", m),
            SmbcDosValue::INode(m) => write!(f, "INODE:{}", m),
            SmbcDosValue::Size(m) => write!(f, "SIZE:{}", m),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// XAttr value given to setxattr
/// Please note that Revision, inode, size, and group cannot be changed individually...
/// and inode and size cannot be changed at all
///
/// Another thing to note: When parsing individual getxattr input to SmbcXAttrValue,
/// the Sid Value is not set (due to how getxattr of acl:sid outputs)
/// You will need to set that yourself
/// the parser will by default always return NUMERIC ACE (if given individual ACE to
/// parse from getxattr)
pub enum SmbcXAttrValue {
    /// acl, acl+
    Ace(ACE),
    /// acl.*, nt_sec_desc.*
    AclAll(Vec<SmbcAclValue>),
    /// dos_attr.*
    DosAll(Vec<SmbcDosValue>),
    /// owner, group
    Sid(Sid),
    /// owner+, group+  
    SidPlus(String),
    /// revision, a_time, c_time, m_time, inode
    Unsigned(u64),
    /// mode
    Mode(DosMode),
    /// size
    Signed(i64),
    /// all attribute values (system.*)
    All(Vec<SmbcAclValue>, Vec<SmbcDosValue>),
}

/// function used to format a vector of items delimited by some string
pub fn separated<D: fmt::Display>(iter: &[D], delimiter: &str) -> String {
    let mut delim_separated = String::new();
    for num in &iter[0..iter.len() - 1] {
        delim_separated.push_str(&format!("{}", num));
        delim_separated.push_str(delimiter);
    }
    delim_separated.push_str(&iter[iter.len() - 1].to_string());
    delim_separated
}

impl fmt::Display for SmbcXAttrValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmbcXAttrValue::Ace(s) => s.fmt(f),
            SmbcXAttrValue::AclAll(s) => {
                let comma_separated = separated(s, "\n");
                write!(f, "{}", comma_separated)
            }
            SmbcXAttrValue::DosAll(s) => {
                let comma_separated = separated(s, "\t");
                write!(f, "{}", comma_separated)
            }
            SmbcXAttrValue::All(a, d) => {
                let mut comma_separated = separated(a, "\n");
                comma_separated.push_str(",");
                let dcomma_separated = separated(d, "\t");
                comma_separated.push_str(&dcomma_separated);
                write!(f, "{}", comma_separated)
            }
            SmbcXAttrValue::Sid(s) => s.fmt(f),
            SmbcXAttrValue::SidPlus(s) => write!(f, "{}", s),
            SmbcXAttrValue::Unsigned(s) => write!(f, "{}", s),
            SmbcXAttrValue::Mode(m) => write!(f, "{:x}", m.bits()),
            SmbcXAttrValue::Signed(s) => write!(f, "{}", s),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// Excludable attributes from a .* call are:
/// nt_sec_desc.revision
/// nt_sec_desc.owner
/// nt_sec_desc.group
/// nt_sec_desc.acl
/// dos_attr.mode
/// dos_attr.size
/// dos_attr.inode
/// dos_attr.c_time
/// dos_attr.a_time
/// dos_attr.m_time
///
/// PLEASE NOTE that you cannot exclude all sub attributes of
/// a .* call. You will get an error
///
pub enum SmbcExclude {
    /// Exclude Revision
    Rev,
    /// Exclude Owner
    Own,
    /// Exclude Group
    Grp,
    /// Exclude Acl list
    Acl,
    /// Exclude Dos Mode
    Mod,
    /// Exclude Size
    Siz,
    /// Exclude CTime
    Ctm,
    /// Exclude ATime
    Atm,
    /// Exclude MTime
    Mtm,
    /// Exclude INode
    Ino,
}

impl fmt::Display for SmbcExclude {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmbcExclude::Rev => write!(f, "nt_sec_desc.revision"),
            SmbcExclude::Own => write!(f, "nt_sec_desc.owner"),
            SmbcExclude::Grp => write!(f, "nt_sec_desc.group"),
            SmbcExclude::Acl => write!(f, "nt_sec_desc.acl"),
            SmbcExclude::Mod => write!(f, "dos_attr.mode"),
            SmbcExclude::Siz => write!(f, "dos_attr.size"),
            SmbcExclude::Ctm => write!(f, "dos_attr.c_time"),
            SmbcExclude::Atm => write!(f, "dos_attr.a_time"),
            SmbcExclude::Mtm => write!(f, "dos_attr.m_time"),
            SmbcExclude::Ino => write!(f, "dos_attr.inode"),
        }
    }
}

#[derive(Debug, Clone)]
/// A directory entry
pub struct SmbcDirEntry {
    /// the filetype of the entry
    pub s_type: SmbcType,
    /// comment associated with the entry
    pub comment: String,
    /// path of/to the entry
    pub path: PathBuf,
}

#[derive(Clone)]
/// A samba directory
pub struct SmbcDirectory {
    /// the samba context
    smbc: Arc<Mutex<SmbcPtr>>,
    /// handle to the directory
    handle: *mut SMBCFILE,
    pub lseekdir_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, dir: *mut SMBCFILE, offset: off_t) -> c_int),
    pub readdir_fn: (unsafe extern "C" fn(c: *mut SMBCCTX, dir: *mut SMBCFILE) -> *mut smbc_dirent),
    pub telldir_fn: (unsafe extern "C" fn(c: *mut SMBCCTX, dir: *mut SMBCFILE) -> off_t),
}

impl Drop for SmbcDirectory {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            let ptr = match self.smbc.lock() {
                Ok(p) => p,
                Err(e) => {
                    error!("Poisoned mutex {:?}", e);
                    panic!("POISONED MUTEX {:?}!!!!", e)
                }
            };

            trace!(target: "smbc", "closing smbc file");
            unsafe {
                smbc_getFunctionClosedir(ptr.0).map(|f| f(ptr.0, self.handle));
            }
        }
    }
}

#[derive(Clone)]
/// A samba file
pub struct SmbcFile {
    /// the samba context
    smbc: Arc<Mutex<SmbcPtr>>,
    /// handle to the file
    fd: *mut SMBCFILE,
    pub fstat_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, file: *mut SMBCFILE, st: *mut stat) -> c_int),
    pub ftruncate_fn:
        (unsafe extern "C" fn(c: *mut SMBCCTX, f: *mut SMBCFILE, size: off_t) -> c_int),
    pub lseek_fn: (unsafe extern "C" fn(
        c: *mut SMBCCTX,
        file: *mut SMBCFILE,
        offset: off_t,
        whence: c_int,
    ) -> off_t),
    pub read_fn: (unsafe extern "C" fn(
        c: *mut SMBCCTX,
        file: *mut SMBCFILE,
        buf: *mut c_void,
        count: usize,
    ) -> isize),
    pub write_fn: (unsafe extern "C" fn(
        c: *mut SMBCCTX,
        file: *mut SMBCFILE,
        buf: *const c_void,
        count: usize,
    ) -> isize),
}

impl Drop for SmbcFile {
    fn drop(&mut self) {
        if !self.fd.is_null() {
            let ptr = match self.smbc.lock() {
                Ok(p) => p,
                Err(e) => {
                    error!("Poisoned mutex {:?}", e);
                    panic!("POISONED MUTEX {:?}!!!!", e)
                }
            };

            unsafe {
                smbc_getFunctionClose(ptr.0).map(|f| f(ptr.0, self.fd));
            }
        }
    }
}

impl Smbc {
    pub fn set_data(wg: String, un: String, pw: String) {
        let mut data = match user_data.lock() {
            Ok(e) => e,
            Err(e) => panic!("Error {:?} Mutex poisoned!", e),
        };
        data[0] = wg;
        data[1] = un;
        data[2] = pw;
    }
    /// new function with Authentication built in
    pub fn new_with_auth(level: u32) -> Result<Smbc> {
        unsafe {
            smbc_init(Some(Self::set_data_wrapper), level as i32);
            let ctx = result_from_ptr_mut(smbc_new_context())?;
            smbc_setOptionDebugToStderr(ctx, 1);
            smbc_setOptionUserData(ctx, Self::auth_wrapper as *mut c_void);
            smbc_setFunctionAuthData(ctx, Some(Self::set_data_wrapper));
            smbc_setFunctionAuthDataWithContext(ctx, Some(Self::auth_wrapper));
            smbc_setOptionOneSharePerServer(ctx, 1);
            let data = match user_data.lock() {
                Ok(e) => e,
                Err(e) => panic!("Error {:?}, Mutex poisoned!", e),
            };
            let (wg, un, pw) = (
                match data.get(0) {
                    Some(e) => e.to_string(),
                    None => "WORKGROUP".to_string(),
                },
                match data.get(1) {
                    Some(e) => e.to_string(),
                    None => "guest".to_string(),
                },
                match data.get(2) {
                    Some(e) => e.to_string(),
                    None => "".to_string(),
                },
            );
            smbc_set_credentials_with_fallback(
                ctx,
                wg.as_ptr() as *const c_char,
                un.as_ptr() as *const c_char,
                pw.as_ptr() as *const c_char,
            );
            smbc_setDebug(ctx, level as i32);
            let ptr: *mut SMBCCTX = match result_from_ptr_mut(smbc_init_context(ctx)) {
                Ok(p) => p,
                Err(e) => {
                    trace!(target: "smbc", "smbc_init failed {:?}", e);
                    smbc_free_context(ctx, 1 as c_int);
                    ptr::null_mut()
                }
            };
            smbc_set_context(ptr);
            Ok(Smbc {
                context: Arc::new(Mutex::new(SmbcPtr(ptr))),
                chmod_fn: try_ufnrc!(smbc_getFunctionChmod <- ptr),
                close_fn: try_ufnrc!(smbc_getFunctionClose <- ptr),
                closedir_fn: try_ufnrc!(smbc_getFunctionClosedir <- ptr),
                creat_fn: try_ufnrc!(smbc_getFunctionCreat <- ptr),
                fstat_fn: try_ufnrc!(smbc_getFunctionFstat <- ptr),
                fstatvfs_fn: try_ufnrc!(smbc_getFunctionFstatVFS <- ptr),
                fstatdir_fn: try_ufnrc!(smbc_getFunctionFstatdir <- ptr),
                ftruncate_fn: try_ufnrc!(smbc_getFunctionFtruncate <- ptr),
                getdents_fn: try_ufnrc!(smbc_getFunctionGetdents <- ptr),
                getxattr_fn: try_ufnrc!(smbc_getFunctionGetxattr <- ptr),
                listxattr_fn: try_ufnrc!(smbc_getFunctionListxattr <- ptr),
                lseek_fn: try_ufnrc!(smbc_getFunctionLseek <- ptr),
                lseekdir_fn: try_ufnrc!(smbc_getFunctionLseekdir <- ptr),
                mkdir_fn: try_ufnrc!(smbc_getFunctionMkdir <- ptr),
                open_fn: try_ufnrc!(smbc_getFunctionOpen <- ptr),
                opendir_fn: try_ufnrc!(smbc_getFunctionOpendir <- ptr),
                read_fn: try_ufnrc!(smbc_getFunctionRead <- ptr),
                readdir_fn: try_ufnrc!(smbc_getFunctionReaddir <- ptr),
                removexattr_fn: try_ufnrc!(smbc_getFunctionRemovexattr <- ptr),
                rename_fn: try_ufnrc!(smbc_getFunctionRename <- ptr),
                rmdir_fn: try_ufnrc!(smbc_getFunctionRmdir <- ptr),
                setxattr_fn: try_ufnrc!(smbc_getFunctionSetxattr <- ptr),
                stat_fn: try_ufnrc!(smbc_getFunctionStat <- ptr),
                statvfs_fn: try_ufnrc!(smbc_getFunctionStatVFS <- ptr),
                telldir_fn: try_ufnrc!(smbc_getFunctionTelldir <- ptr),
                unlink_fn: try_ufnrc!(smbc_getFunctionUnlink <- ptr),
                utimes_fn: try_ufnrc!(smbc_getFunctionUtimes <- ptr),
                write_fn: try_ufnrc!(smbc_getFunctionWrite <- ptr),
            })
        }
    }

    /// Auth wrapper
    extern "C" fn auth_wrapper(
        _ctx: *mut SMBCCTX,
        srv: *const c_char,
        shr: *const c_char,
        wg: *mut c_char,
        _wglen: c_int,
        un: *mut c_char,
        _unlen: c_int,
        pw: *mut c_char,
        _pwlen: c_int,
    ) {
        unsafe {
            let t_srv = CStr::from_ptr(srv);
            let t_shr = CStr::from_ptr(shr);
            let _srv = t_srv.as_ptr();
            let _shr = t_shr.as_ptr();
            trace!(target: "smbc", "authenticating on {:?}\\{:?}", &t_srv, &t_shr);

            //either use the provided credentials or the default guest
            let data = match user_data.lock() {
                Ok(e) => e,
                Err(e) => panic!("Error {:?}, Mutex poisoned!", e),
            };
            let (workgroup, username, password) = (
                match data.get(0) {
                    Some(e) => e.to_string(),
                    None => "WORKGROUP".to_string(),
                },
                match data.get(1) {
                    Some(e) => e.to_string(),
                    None => "guest".to_string(),
                },
                match data.get(2) {
                    Some(e) => e.to_string(),
                    None => "".to_string(),
                },
            );
            let (wg_ptr, un_ptr, pw_ptr) = (
                CString::from_vec_unchecked(workgroup.clone().into_bytes()),
                CString::from_vec_unchecked(username.clone().into_bytes()),
                CString::from_vec_unchecked(password.clone().into_bytes()),
            );
            trace!(target: "smbc", "cred: {:?}\\{:?} {:?}", &workgroup, &username, &password);
            let (wglen, unlen, pwlen) = (workgroup.len(), username.len(), password.len());

            strncpy(wg, wg_ptr.as_ptr(), wglen);
            strncpy(un, un_ptr.as_ptr(), unlen);
            strncpy(pw, pw_ptr.as_ptr(), pwlen);
        }
    }
    extern "C" fn set_data_wrapper(
        srv: *const c_char,
        shr: *const c_char,
        wg: *mut c_char,
        _wglen: c_int,
        un: *mut c_char,
        _unlen: c_int,
        pw: *mut c_char,
        _pwlen: c_int,
    ) {
        unsafe {
            let t_srv = CStr::from_ptr(srv);
            let t_shr = CStr::from_ptr(shr);
            let _srv = t_srv.as_ptr();
            let _shr = t_shr.as_ptr();
            trace!(target: "smbc", "authenticating on {:?}\\{:?}", &t_srv, &t_shr);
            //either use the provided credentials or the default guest
            let data = match user_data.lock() {
                Ok(e) => e,
                Err(e) => panic!("Error {:?}, Mutex poisoned!", e),
            };
            let (workgroup, username, password) = (
                match data.get(0) {
                    Some(e) => e.to_string(),
                    None => "WORKGROUP".to_string(),
                },
                match data.get(1) {
                    Some(e) => e.to_string(),
                    None => "guest".to_string(),
                },
                match data.get(2) {
                    Some(e) => e.to_string(),
                    None => "".to_string(),
                },
            );
            let (wg_ptr, un_ptr, pw_ptr) = (
                CString::from_vec_unchecked(workgroup.clone().into_bytes()),
                CString::from_vec_unchecked(username.clone().into_bytes()),
                CString::from_vec_unchecked(password.clone().into_bytes()),
            );
            trace!(target: "smbc", "cred: {:?}\\{:?} {:?}", &workgroup, &username, &password);
            let (wglen, unlen, pwlen) = (workgroup.len(), username.len(), password.len());

            strncpy(wg, wg_ptr.as_ptr(), wglen);
            strncpy(un, un_ptr.as_ptr(), unlen);
            strncpy(pw, pw_ptr.as_ptr(), pwlen);
        }
    }

    ///
    /// Create a file on an SMB server.
    ///
    /// Same as calling smbc_open() with flags = O_CREAT|O_WRONLY|O_TRUNC
    ///
    /// @param furl      The smb url of the file to be created
    ///
    /// @param mode      NOTE: mode does not do anything for file permissions.  
    ///
    /// @return          SmbcFile, Error with errno set:
    ///                  - ENOMEM  Out of memory
    ///                  - EINVAL if an invalid parameter passed, like no
    ///                  file, or smbc_init not called.
    ///                  - EEXIST  pathname already exists and O_CREAT and
    ///                  O_EXCL were used.
    ///                  - EISDIR  pathname  refers  to  a  directory  and
    ///                   the access requested involved writing.
    ///                   - EACCES  The requested access to the file is not
    ///                   allowed
    ///                   - ENOENT  A directory component in pathname does
    ///                   not exist.
    ///                   - ENODEV The requested share does not exist.
    ///
    pub fn create(&mut self, path: &Path, mode: Mode) -> Result<SmbcFile> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        trace!(target: "smbc", "Attempting to retrieve context");
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        trace!(target: "smbc", "Sucessfully retrieved context, attempting to apply function");

        unsafe {
            let fd =
                result_from_ptr_mut((self.creat_fn)(ptr.0, path.as_ptr(), mode.bits() as mode_t))?;
            trace!(target: "smbc", "Returned value is {:?}", fd);
            if (fd as i64) < 0 {
                trace!(target: "smbc", "Error: neg fd");
            }
            Ok(SmbcFile {
                smbc: Arc::clone(&self.context),
                fd,
                fstat_fn: self.fstat_fn,
                ftruncate_fn: self.ftruncate_fn,
                lseek_fn: self.lseek_fn,
                read_fn: self.read_fn,
                write_fn: self.write_fn,
            })
        }
    }

    /// Chmod
    /// chmod changes the DOS attributes of the input filepath
    /// NOTE: chmod only works if, in the smb.conf file,
    /// store dos mode = yes and vfs objects = yes,
    /// or store dos mode = yes,
    /// or neither of those attributes are set in the config file.
    /// You may only change/add/modify the following attributes:
    /// R - ReadOnly
    /// A - Archive
    /// S - System
    /// H - Hidden
    /// (Note that, if none of those are set, you will have either N
    /// for normal file, or D for directory)
    /// The four above attributes can only be modified if you:
    /// Have an appropriate mask in your config, and their
    /// respective map <attribute> = yes
    /// R => map readonly = yes
    /// A => map archive = yes
    /// S => map system = yes
    /// H => map Hidden = yes
    ///
    /// For more details on how chmod works, please go to:
    /// https://ftp.samba.org/pub/pub/unpacked/SOC/2005/SAMBA_3_0/source/libsmb/libsmbclient.c
    ///
    pub fn chmod(&self, path: &Path, mode: Mode) -> Result<()> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        trace!(target: "smbc", "Attempting to retrieve context");
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        trace!(target: "smbc", "Sucessfully retrieved context, attempting to apply function");

        unsafe {
            to_result_with_le((self.chmod_fn)(ptr.0, path.as_ptr(), mode.bits() as mode_t))?;
        }
        trace!(target: "smbc", "Chmod_fn ran");
        Ok(())
    }
    ///@ingroup file
    /// Open a file on an SMB server.
    ///
    /// @param path      The smb url of the file to be opened.
    ///
    /// @param flags     Is one of O_RDONLY, O_WRONLY or O_RDWR which
    ///                  request opening  the  file  read-only,write-only
    ///                  or read/write. flags may also be bitwise-or'd with
    ///                  one or  more of  the following:
    ///                  O_CREAT - If the file does not exist it will be
    ///                  created.
    ///                  O_EXCL - When  used with O_CREAT, if the file
    ///                  already exists it is an error and the open will
    ///                  fail.
    ///                  O_TRUNC - If the file already exists it will be
    ///                  truncated.
    ///                  O_APPEND The  file  is  opened  in  append mode
    ///
    /// @param mode      mode specifies the permissions to use if a new
    ///                  file is created.  It  is  modified  by  the
    ///                  process's umask in the usual way: the permissions
    ///                  of the created file are (mode & ~umask)
    ///                  Not currently use, but there for future use.
    ///                  We will map this to SYSTEM, HIDDEN, etc bits
    ///                  that reverses the mapping that smbc_fstat does.
    ///
    /// @return          Valid file handle, < 0 on error with errno set:
    ///                  - ENOMEM  Out of memory
    ///                  - EINVAL if an invalid parameter passed, like no
    ///                  file, or smbc_init not called.
    ///                  - EEXIST  pathname already exists and O_CREAT and
    ///                  O_EXCL were used.
    ///                  - EISDIR  pathname  refers  to  a  directory  and
    ///                  the access requested involved writing.
    ///                  - EACCES  The requested access to the file is not
    ///                  allowed
    ///                  - ENODEV The requested share does not exist
    ///                  - ENOTDIR A file on the path is not a directory
    ///                  - ENOENT  A directory component in pathname does
    ///                  not exist.
    ///
    /// @note            This call uses an underlying routine that may create
    ///                  a new connection to the server specified in the URL.
    ///                  If the credentials supplied in the URL, or via the
    ///                  auth_fn in the smbc_init call, fail, this call will
    ///                  try again with an empty username and password. This
    ///                  often gets mapped to the guest account on some machines.
    ///                  Mode doesn't DO anything for file permissions.
    ///                  the mode variable is never used internally,
    ///                  so the file is always opened with default, or
    ///                  it's own permissions.  It does keep whatever
    ///                  UNIX permissions the file has intact though.
    ///                 
    ///
    pub fn open(&self, path: &Path, flags: OFlag, mode: Mode) -> Result<SmbcFile> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        trace!(target: "smbc", "Attempting to retrieve context");
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };

        trace!(target: "smbc", "Sucessfully retrieved context, attempting to apply function");
        let fd = result_from_ptr_mut(unsafe {
            (self.open_fn)(ptr.0, path.as_ptr(), flags.bits(), mode.bits())
        })?;
        if (fd as i64) < 0 {
            trace!(target: "smbc", "neg fd");
        }
        Ok(SmbcFile {
            smbc: Arc::clone(&self.context),
            fd,
            fstat_fn: self.fstat_fn,
            ftruncate_fn: self.ftruncate_fn,
            lseek_fn: self.lseek_fn,
            read_fn: self.read_fn,
            write_fn: self.write_fn,
        })
    }

    ///
    /// Open a directory used to obtain directory entries.
    ///
    /// @param path      The smb url of the directory to open
    ///
    /// @return          Valid directory handle. < 0 on error with errno set:
    ///                  - EACCES Permission denied.
    ///                  - EINVAL A NULL file/URL was passed, or the URL would
    ///                  not parse, or was of incorrect form or smbc_init not
    ///                  called.
    ///                  - ENOENT durl does not exist, or name is an
    ///                  - ENOMEM Insufficient memory to complete the
    ///                  operation.
    ///                  - ENOTDIR name is not a directory.
    ///                  - EPERM the workgroup could not be found.
    ///                  - ENODEV the workgroup or server could not be found.
    ///
    pub fn opendir(&self, path: &Path) -> Result<SmbcDirectory> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        trace!(target: "smbc", "Attempting to retrieve context");
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        trace!(target: "smbc", "Sucessfully retrieved context, attempting to apply function");
        let handle = result_from_ptr_mut(unsafe { (self.opendir_fn)(ptr.0, path.as_ptr()) })?;
        if (handle as i64) < 0 {
            trace!(target: "smbc", "Error: neg directory fd");
        }
        Ok(SmbcDirectory {
            smbc: Arc::clone(&self.context),
            handle,
            lseekdir_fn: self.lseekdir_fn,
            readdir_fn: self.readdir_fn,
            telldir_fn: self.telldir_fn,
        })
    }

    ///
    /// Please NOTE that MODE does not matter, since the
    /// function never actually uses the input mode...
    /// See https://ftp.samba.org/pub/pub/unpacked/SOC/2005/SAMBA_3_0/source/libsmb/libsmbclient.cg
    /// for details
    ///
    pub fn mkdir(&self, path: &Path, mode: Mode) -> Result<()> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        trace!(target: "smbc", "Attempting to retrieve context");
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        trace!(target: "smbc", "Sucessfully retrieved context, attempting to apply function");
        let handle =
            to_result_with_le(unsafe { (self.mkdir_fn)(ptr.0, path.as_ptr(), mode.bits()) })?;
        if i64::from(handle) < 0 {
            trace!(target: "smbc", "Error: neg directory fd");
        }
        Ok(())
    }

    ///
    /// Rename or move a file or directory.
    ///
    /// @param oldpath   The original smb url (source url) of file or
    ///                  directory to be moved
    ///
    /// @param newpath   The new smb url (destination url) of the file
    ///                  or directory after the move.  Currently nurl must
    ///                  be on the same share as ourl.
    ///
    /// @return          Nothing on success, Error with errno set:
    ///                  - EISDIR nurl is an existing directory, but ourl is
    ///                  not a directory.
    ///                  - EEXIST nurl is  a  non-empty directory,
    ///                  i.e., contains entries other than "." and ".."
    ///                  - EINVAL The  new  url  contained  a path prefix
    ///                  of the old, or, more generally, an  attempt was
    ///                  made  to make a directory a subdirectory of itself
    /// 		         or smbc_init not called.
    ///                  - ENOTDIR A component used as a directory in ourl
    ///                  or nurl path is not, in fact, a directory.  Or,
    ///                  ourl  is a directory, and newpath exists but is not
    ///                  a directory.
    ///                  - EACCES or EPERM Write access to the directory
    ///                  containing ourl or nurl is not allowed for the
    ///                  process's effective uid,  or  one of the
    ///                  directories in ourl or nurl did not allow search
    ///                  (execute) permission,  or ourl  was  a  directory
    ///                  and did not allow write permission.
    ///                  - ENOENT A  directory component in ourl or nurl
    ///                  does not exist.
    ///                  - EXDEV Rename across shares not supported.
    ///                  - ENOMEM Insufficient kernel memory was available.
    ///                  - EEXIST The target file, nurl, already exists.
    ///
    pub fn rename(&self, oldpath: &Path, newpath: &Path) -> Result<()> {
        let oldpath = CString::new(oldpath.as_os_str().as_bytes())?;
        let newpath = CString::new(newpath.as_os_str().as_bytes())?;
        trace!(target: "smbc", "Attempting to retrieve context");
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        trace!(target: "smbc", "Successfully retrieved context, attempting to apply function");
        to_result_with_le(unsafe {
            (self.rename_fn)(ptr.0, oldpath.as_ptr(), ptr.0, newpath.as_ptr())
        })?;
        Ok(())
    }

    ///
    /// rmdir
    ///
    /// remove a directory
    ///
    /// @param path     The smb url of the directory to remove
    ///
    /// @return         nothing on success, error with errno set:
    ///                 - EACCES or EPERM Write access to the directory
    ///                 containing pathname was not allowed.
    ///                 - EINVAL path is NULL or smbc_init not called.
    ///                 - ENOENT A directory component in pathname does not
    ///                 exist.
    ///                 - ENOTEMPTY directory contains entries.
    ///                 - ENOMEM Insufficient kernel memory was available.
    ///
    pub fn rmdir(&self, path: &Path) -> Result<()> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        trace!(target: "smbc", "Attempting to retreive context");
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        trace!(target: "smbc", "Successfully retrieved context, attempting to apply function");
        to_result_with_le(unsafe { (self.rmdir_fn)(ptr.0, path.as_ptr()) })?;
        Ok(())
    }

    ///
    /// stat
    /// stat returns the meta attributes of a file/directory
    /// NOTE:
    /// block size is always 512
    /// nlink with be either 2 if the file is a directory or 1 otherwise
    /// blocks is (size+511)/512
    ///
    /// mode values depend on what attributes are active in smb.conf
    /// file modes will always by default have a minimum mode of
    /// 100444, directory 40555.
    ///
    /// if map archive = yes, you can add the XUSR flag to mode, (USR can be 4, 5)
    /// if map readonly = yes, you can add the WUSR flag to mode, (USR can be 4, 5(dir only), 6)
    /// (if both, USR permissions can be 4, 5, 6, or 7)
    /// if map system = yes, you can add the XGRP flag to mode, (GRP can be 4, 5)
    /// if map hidden = yes, you can add the XOTH flag to mode, (OTH can be 4, 5)
    ///
    /// m_time, c_time, a_time return the number of seconds since epoch
    ///
    /// See https://ftp.samba.org/pub/pub/unpacked/SOC/2005/SAMBA_3_0/source/libsmb/libsmbclient.c
    /// for details (you'll be surprised at how much of this is hard coded...)
    ///
    pub fn stat(&self, path: &Path) -> Result<stat> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let mut stat_buf: stat = unsafe { zeroed::<stat>() };
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let res =
            to_result_with_le(unsafe { (self.stat_fn)(ptr.0, path.as_ptr(), &mut stat_buf) })?;
        if i64::from(res) < 0 {
            trace!(target: "smbc", "stat failed");
        }
        Ok(stat_buf)
    }

    /**
     * NOTE!
     * DOES NOT WORK
     */
    /*pub fn statvfs(&self, path: &Path) -> Result<statvfs> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let mut stat_buf: statvfs = unsafe { zeroed::<statvfs>() };
        let statvfs_fn = try_ufnrc!(smbc_getFunctionStatVFS <- self.context);
        let res = to_result_with_le(unsafe {
            statvfs_fn(self.context.0, path.into_raw(), &mut stat_buf)
        })?;
        if (res as i64) < 0 {
            trace!(target: "smbc", "statvfs failed");
        }
        Ok(stat_buf)
    }*/

    ///@ingroup directory
    /// Unlink (delete) a file or directory.
    ///
    /// @param path      The smb url of the file to delete
    ///
    /// @return          Nothing on success, Error with errno set:
    ///                  - EACCES or EPERM Write  access  to the directory
    ///                  containing pathname is not allowed or one
    ///                  of  the  directories in pathname did not allow
    ///                  search (execute) permission
    ///                  - ENOENT A directory component in pathname does
    ///                  not exist
    ///                  - EINVAL NULL was passed in the file param or
    /// 		           smbc_init not called.
    ///                  - EACCES You do not have access to the file
    ///                  - ENOMEM Insufficient kernel memory was available
    ///
    pub fn unlink(&self, path: &Path) -> Result<()> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };

        unsafe {
            to_result_with_le((self.unlink_fn)(ptr.0, path.as_ptr()))?;
        }
        Ok(())
    }

    ///
    /// Change the last modification time on a file
    ///
    /// @param path      The smb url of the file or directory to change
    ///                  the modification time of
    ///
    /// @param tbuf      An array of two timeval structures which contains,
    ///                  respectively, the desired access and modification times.
    ///                  NOTE: Only the tv_sec field off each timeval structure is
    ///                  used.  The tv_usec (microseconds) portion is ignored.
    ///
    /// @return          Nothing on success, Error with errno set:
    ///                  - EINVAL The client library is not properly initialized
    ///                  - EPERM  Permission was denied.
    ///
    pub fn utimes(&self, path: &Path, tbuf: &mut Vec<timeval>) -> Result<()> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        unsafe {
            to_result_with_le((self.utimes_fn)(ptr.0, path.as_ptr(), tbuf.as_mut_ptr()))?;
        }
        Ok(())
    }

    /// Get extended attributes for a file.
    ///
    /// @param path       The smb url of the file or directory to get extended
    ///                  attributes for.
    ///
    /// @param attr      The name of an attribute to be retrieved.  Names are of
    ///                  one of the following forms:
    ///                     system.nt_sec_desc.<attribute name>
    ///                     system.nt_sec_desc.*
    ///                     system.nt_sec_desc.*+
    ///                  where <attribute name> is one of:
    ///                     revision
    ///                     owner
    ///                     owner+
    ///                     group
    ///                     group+
    ///                     acl:<sid>
    ///                     acl+:<sid>
    ///                  In the forms "system.nt_sec_desc.*" and
    ///                 "system.nt_sec_desc.*+", the asterisk and plus signs are
    ///                 literal, i.e. the string is provided exactly as shown, an
    ///                 the value parameter will return a complete security
    ///                  commas, or newlines (not spaces!).
    ///                 The plus sign ('+') indicates that SIDs should be mapped
    ///                  to names.  Without the plus sign, SIDs are not mapped;
    ///                 rather they are simply converted to a string format.
    ///                 or:
    ///                     system.dos_attr.<attribute name>
    ///                     system.dos_attr.*
    ///                  where <attribute name> is one of:
    ///                     mode
    ///                     c_time
    ///                     a_time
    ///                     m_time
    ///                     inode
    ///                     size
    ///                 or
    ///                     system.*
    ///                     system.*+
    ///                 The * attribute will get all values of a set (so system.* will
    ///                 return all DOS and ACL attributes, system.dos_attr.* all DOS
    ///                 attributes, etc.).  The commands with * may also exclude elements
    ///                 with ! delimiters (ex: system.*!nt_sec_desc.acl!dos_attr.mode will
    ///                 return all attributes excluding acl and mode)
    ///                 Use the SmbcXattr enum to build your input.
    ///
    /// @return          0 on success, < 0 on error with errno set:
    ///                  - EINVAL  The client library is not properly initialized
    ///                            or one of the parameters is not of a correct
    ///                           form
    ///                  - ENOMEM No memory was available for internal needs
    ///                  - EEXIST  If the attribute already exists and the flag
    ///                            SMBC_XATTR_FLAG_CREAT was specified
    ///                  - ENOATTR If the attribute does not exist and the flag
    ///                            SMBC_XATTR_FLAG_REPLACE was specified
    ///                  - EPERM   Permission was denied.
    ///                  - ENOTSUP The referenced file system does not support
    ///                            extended attributes
    ///
    /// NOTE:
    ///     system.nt_sec_desc.acl(+):sid will ONLY return the acetype, aceflag, and
    ///     xattr.  When parsing the output to SmbcXAttrValue, you must set the
    ///     SidType Sid manually, otherwise it will be NONE.  Also, the parser will
    ///     always return a NUMERIC ACE
    ///
    pub fn getxattr(&self, path: &Path, attr: &SmbcXAttr) -> Result<Vec<u8>> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let name = CString::new(format!("{}", attr).as_bytes())?;
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        // Set your buffer to capacity len here
        let len = to_result_with_le(unsafe {
            (self.getxattr_fn)(
                ptr.0,
                path.as_ptr(),
                name.as_ptr(),
                vec![].as_ptr() as *const _,
                0,
            )
        })? + 1;
        trace!(target: "smbc", "Sizing buffer to {}", len);
        let mut value: Vec<u8> = Vec::with_capacity(len as usize);
        if i64::from(len) < 0 {
            trace!(target: "smbc", "getxattr failed");
        }
        let res = to_result_with_le(unsafe {
            (self.getxattr_fn)(
                ptr.0,
                path.as_ptr(),
                name.as_ptr(),
                value.as_ptr() as *const _,
                len as _,
            )
        })?;
        if i64::from(res) < 0 {
            trace!(target: "smbc", "getxattr failed");
        }
        unsafe {
            value.set_len(len as usize);
        }

        Ok(value)
    }

    ///
    /// While this function is supposed to list only the applicable attributes
    /// of a file/directory, this funciton always returns all attribute names
    /// supported by NT file systems, regardless of whether the referenced
    /// file system supports extended attributes
    ///
    pub fn listxattr(&self, path: &Path) -> Result<Vec<u8>> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        // Set your buffer to capacity len here
        let temp: Vec<u8> = vec![];
        let len = to_result_with_le(unsafe {
            (self.listxattr_fn)(ptr.0, path.as_ptr(), temp.as_ptr() as *mut c_char, 0)
        })?;
        trace!(target: "smbc", "Sizing buffer to {}", len);
        let mut value: Vec<u8> = Vec::with_capacity(len as usize);
        if i64::from(len) < 0 {
            trace!(target: "smbc", "listxattr failed");
        }

        let res = to_result_with_le(unsafe {
            (self.listxattr_fn)(
                ptr.0,
                path.as_ptr(),
                value.as_ptr() as *mut c_char,
                len as _,
            )
        })?;
        if i64::from(res) < 0 {
            trace!(target: "smbc", "listxattr failed");
        }
        unsafe {
            value.set_len(len as usize);
        }
        Ok(value)
    }

    ///
    /// NOTE: removexattr only works for the following inputs:
    /// system.nt_sec_desc.*
    /// system.nt_sec_desc.*+
    /// system.nt_sec_desc.acl:sid
    /// system.nt_sec_desc.acl+:sid
    ///
    /// In order for removexattr to run, you must have in your config file:
    /// store dos attributes = yes and vfs objects = yes
    /// or vfs objects = yes
    ///
    /// PLEASE NOTE: NAMED attributes for removexattr will only accept the fields
    /// "FULL", "CHANGE", and "READ" (same as setxattr), otherwise it will segfault
    /// (There's nothing I can do about this, Samba manages to get a segfault somehow...)
    ///
    /// Oh, and the reason why revision, owner(+), group(+) don't work is because of how sec_desc_parse works.  
    /// See https://ftp.samba.org/pub/pub/unpacked/SOC/2005/SAMBA_3_0/source/libsmb/libsmbclient.c
    /// for details
    ///
    pub fn removexattr(&self, path: &Path, attr: &SmbcXAttr) -> Result<()> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let name = CString::new(format!("{}", attr).as_bytes())?;
        //let name = CString::new(name.to_string().as_bytes())?;
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        unsafe {
            to_result_with_le((self.removexattr_fn)(ptr.0, path.as_ptr(), name.as_ptr()))?;
        }
        Ok(())
    }

    ///
    /// Please note that if your file has removed all acl attributes, setxattr
    /// commands may not work.  
    ///
    /// In your config file, if you do not have map archive = yes, map hidden = yes,
    /// map readonly = yes, map system = yes, some of your commands may not work,
    /// such as changing mode.  You may want to have both store dos attributes = yes
    /// and vfs objects = yes if you want both remove and setxattr to work.  
    ///
    /// As such, please note that your file ACL permissions do in fact effect
    /// whether or not you can make changes to a file as well.
    ///
    /// @note: setxattr on system.nt_sec_desc.group(+),
    ///                   system.dos_attr.size,
    ///                   system.dos_attr.inode, do not work
    ///        Also, setxattr on system.dos_attr.* (it sets everything except size and inode...)
    ///        Also, NAMED ATTRIBUTES for ACL's only work with the mask inputs
    ///        FULL, READ, and CHANGE.
    ///        Another thing, setxattr on DOSMODE will ONLY set the DOSMODE to N (Normal) if both
    ///        your chmod permissions are set appropriately, AND the xattr acl permissions are
    ///        set correctly (644, or owner RW, group R(W), other R)
    /// See https://ftp.samba.org/pub/pub/unpacked/SOC/2005/SAMBA_3_0/source/libsmb/libsmbcliet.c
    /// for details (It uses the wrong value and therefore tries to change the owner instead
    /// of the group...)
    ///
    /// In general, You will probably have an easier time just setting all of the
    /// ACL attributes at once (removing extra), than individually considereing
    /// individually, changing group does not work
    ///
    pub fn setxattr(
        &self,
        path: &Path,
        attr: &SmbcXAttr,
        value: &SmbcXAttrValue,
        flags: XAttrFlags,
    ) -> Result<()> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        let len = format!("{}", value).len();
        let name = CString::new(format!("{}", attr).as_bytes())?;
        trace!(target: "smbc", "setxattr name {:?}", name);
        let value = CString::new(format!("{}", value).as_bytes())?;
        trace!(target: "smbc", "setxattr value {:?}, len {}", value, len);
        //let name = CString::new(name.to_string().as_bytes())?;
        let ptr = match self.context.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let res = unsafe {
            (self.setxattr_fn)(
                ptr.0,
                path.as_ptr(),
                name.as_ptr(),
                value.as_ptr() as *const _,
                len as _,
                flags.bits() as _,
            )
        };
        if i64::from(res) < 0 {
            trace!(target: "smbc", "setxattr failed");
            to_result_with_le(res)?;
        }
        Ok(())
    }
}

impl SmbcFile {
    ///
    /// Read from a file using an opened file handle.
    /// @param count   Size of buf in bytes
    ///
    /// @return          Vec of read bytes;
    ///                  ERROR:
    ///                  - EISDIR fd refers to a directory
    ///                  - EBADF  fd  is  not  a valid file descriptor or
    ///                    is not open for reading.
    ///                  - EINVAL fd is attached to an object which is
    ///                    unsuitable for reading, or no buffer passed or
    ///     		       smbc_init not called.
    ///
    /// PLEASE NOTE: read starts from the current file offset
    /// (So if you read 10 bytes, then read again, the second read starts
    /// from the 10th byte) So if you happen to have already read all the bytes,
    /// and have not lseeked back to the beginning,
    /// calling read again will give you an empty vec
    ///
    pub fn fread(&self, count: u64) -> Result<Vec<u8>> {
        let mut buf: Vec<u8> = Vec::with_capacity(count as usize);
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let bytes_read = to_result_with_le(unsafe {
            (self.read_fn)(ptr.0, self.fd, buf.as_mut_ptr() as *mut _, count as usize)
        })?;
        if (bytes_read as i64) < 0 {
            trace!(target: "smbc", "read failed");
        }
        unsafe {
            buf.set_len(bytes_read as usize);
        }
        Ok(buf)
    }

    ///
    /// Write to a file using an opened file handle.
    /// @param buf       Pointer to buffer to recieve read data
    /// @return          Number of bytes written, < 0 on error with errno set:
    ///                  - EISDIR fd refers to a directory.
    ///                  - EBADF  fd  is  not  a valid file descriptor or
    ///                  is not open for reading.
    ///                  - EINVAL fd is attached to an object which is
    ///                  unsuitable for reading, or no buffer passed or
    ///     		     smbc_init not called.
    ///
    /// Please NOTE that fwrite writes from the current file offset
    ///
    pub fn fwrite(&self, buf: &[u8]) -> Result<isize> {
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let bytes_wrote = to_result_with_le(unsafe {
            (self.write_fn)(ptr.0, self.fd, buf.as_ptr() as *const _, buf.len() as _)
        })?;
        if (bytes_wrote as i64) < 0 {
            trace!(target: "smbc", "write failed");
        }
        Ok(bytes_wrote)
    }

    ///
    /// Seek to a specific location in a file.
    /// @param offset    Offset in bytes from whence
    ///
    /// @param whence    A location in the file:
    ///                  - SEEK_SET The offset is set to offset bytes from
    ///                  the beginning of the file
    ///                  - SEEK_CUR The offset is set to current location
    ///                  plus offset bytes.
    ///                  - SEEK_END The offset is set to the size of the
    ///                  file plus offset bytes.
    ///
    /// @return          Upon successful completion, lseek returns the
    ///                  resulting offset location as measured in bytes
    ///                  from the beginning  of the file. Otherwise, a value
    ///                  of (off_t)-1 is returned and errno is set to
    ///                  indicate the error:
    ///                  - EBADF  Fildes is not an open file descriptor.
    ///                  - EINVAL Whence is not a proper value or smbc_init
    ///     		     not called.
    ///
    pub fn lseek(&self, offset: i64, whence: i32) -> Result<off_t> {
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let res = to_result_with_errno(
            unsafe { (self.lseek_fn)(ptr.0, self.fd, offset, whence) },
            EINVAL,
        )?;
        Ok(res as off_t)
    }

    ///
    /// fstat
    /// NOTE: stat notes apply
    /// Please note that fstat called on a directory entry will not work
    /// fstatdir is NOT implemented in the SMB Client library:
    /// See https://ftp.samba.org/pub/pub/unpacked/SOC/2005/SAMBA_3_0/source/libsmb/libsmbclient.c for details.
    /// Please use stat for directory meta attributes
    ///
    pub fn fstat(&self) -> Result<stat> {
        let mut stat_buf: stat = unsafe { zeroed::<stat>() };
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let res = to_result_with_le(unsafe { (self.fstat_fn)(ptr.0, self.fd, &mut stat_buf) })?;
        if i64::from(res) < 0 {
            trace!(target: "smbc", "fstat failed");
        }
        Ok(stat_buf)
    }

    /**
     * Does not Work
     */
    /*pub fn fstatvfs(&self) -> Result<statvfs> {
        let mut stat_buf: statvfs = unsafe { zeroed::<statvfs>() };
        let fstatvfs_fn = try_ufnrc!(smbc_getFunctionFstatVFS <- self.smbc);
        trace!("Applying fstatvfs");
        let res = to_result_with_le(unsafe { fstatvfs_fn(self.smbc.0, self.fd, &mut stat_buf) })?;
        if (res as i64) < 0 {
            trace!(target: "smbc", "statvfs failed");
        }
        Ok(stat_buf)
    }*/

    ///
    /// ftruncate
    /// Truncate a file given a file descriptor
    /// @param size      size to truncate the file to
    ///
    /// @return          Nothing on success;
    ///                  Error:
    ///                  - EBADF  filedes is bad.
    ///                  - EACCES Permission denied.
    ///                  - EBADF fd is not a valid file descriptor
    ///                  - EINVAL Problems occurred in the underlying routines
    /// 		           or smbc_init not called.
    ///                  - ENOMEM Out of memory
    ///
    pub fn ftruncate(&self, size: i64) -> Result<()> {
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        to_result_with_le(unsafe { (self.ftruncate_fn)(ptr.0, self.fd, size as off_t) })?;
        Ok(())
    }
}

///
/// Read trait for SmbcFile
/// pretty much does the same thing as fread above
///
impl Read for SmbcFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        trace!(target: "smbc", "reading file to buf [{:?};{}]", buf.as_ptr(), buf.len());
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let bytes_read = to_result_with_le(unsafe {
            (self.read_fn)(ptr.0, self.fd, buf.as_mut_ptr() as *mut _, buf.len() as _)
        })?;
        Ok(bytes_read as usize)
    }
}

///
/// Write trait for smbcFile
/// Does the same thing as fwrite above
///
impl Write for SmbcFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        trace!(target: "smbc", "writing buf [{:?};{}] to file", buf.as_ptr(), buf.len());
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let bytes_wrote = to_result_with_le(unsafe {
            (self.write_fn)(ptr.0, self.fd, buf.as_ptr() as *const _, buf.len() as _)
        })?;
        Ok(bytes_wrote as usize)
    }

    /// Do nothing for SmbFile
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

///
/// Seek trait for SmbcFile if needed
/// You can just call lseek though...
///
impl Seek for SmbcFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        trace!(target: "smbc", "seeking file {:?}", pos);
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let (whence, off) = match pos {
            SeekFrom::Start(p) => (SEEK_SET, p as off_t),
            SeekFrom::End(p) => (SEEK_END, p as off_t),
            SeekFrom::Current(p) => (SEEK_CUR, p as off_t),
        };
        let res = to_result_with_errno(
            unsafe { (self.lseek_fn)(ptr.0, self.fd, off, whence as i32) },
            EINVAL,
        )?;
        Ok(res as u64)
    }
}

impl SmbcDirectory {
    /**
     * PLEASE NOTE!!!!!
     * fstatdir is NOT implemented in the SMB Client library:
     * See https://ftp.samba.org/pub/pub/unpacked/SOC/2005/SAMBA_3_0/source/libsmb/libsmbclient.c * for details.
     *
     * Therefore this function is useless
     */
    /*
    pub fn fstatdir(&self) -> Result<stat> {
        let mut stat_buf: stat = unsafe { zeroed::<stat>() };
        let fstatdir_fn = try_ufnrc!(smbc_getFunctionFstatdir <- self.smbc);
        let res =
            to_result_with_le(unsafe { fstatdir_fn(self.smbc.0, self.handle, &mut stat_buf) })?;
        if (res as i64) < 0 {
            trace!(target: "smbc", "statdir failed");
        }
        Ok(stat_buf)
    }*/

    /**
     * Iterator implemented, not necessary
     */
    /*pub fn getdents(&mut self, count : i64) -> io::Result<Vec<SmbcDirEntry>>
    {
        let mut dirp: Vec<smbc_dirent> = Vec::with_capacity(count as usize);
        for i in 0..count as usize
        {
            dirp.push(unsafe{zeroed::<smbc_dirent>()});
        }
        trace!("attempting to retrieve getdents function");
        let getdents_fn = try_ufnrc!(smbc_getFunctionGetdents <- self.smbc);
        trace!("Applying getdents function");
        let res = try!(to_result_with_le(
            getdents_fn(self.smbc.0,
                     self.handle,
                     dirp.as_mut_ptr() as *mut smbc_dirent,
                     count as c_int)
        ));
        if (res as i64) < 0 {
            trace!(target: "smbc", "getdents failed");
        }
        let mut dirs : Vec<SmbcDirEntry> = Vec::new();
        for dirent in dirp {
            let filename = unsafe{CStr::from_ptr((dirent.name)[0] as *const i8)};
        let d_type = match SmbcType::from(dirent.smbc_type) {
            Ok(ty) => ty,
            Err(e) => {
                return Err(e);
            }
        };
        let comment = unsafe{CStr::from_ptr((dirent).comment).to_string_lossy().into_owned()};
        dirs.push(SmbcDirEntry{
            s_type : d_type,
            //size : (dirent).dirlen,
            comment : comment,
            path: PathBuf::from(filename.to_string_lossy().into_owned()),
        })
        }
        Ok(dirs)
    }*/
    /***
     * Output hardcoded, do not use
     */
    /*pub fn statvfsdir(&self) -> Result<statvfs> {
        let mut stat_buf: statvfs = unsafe { zeroed::<statvfs>() };
        let fstatvfs_fn = try_ufnrc!(smbc_getFunctionFstatVFS <- self.smbc);
        trace!("Applying fstatvfs");
        let res = to_result_with_le(unsafe { fstatvfs_fn(self.smbc.0, self.handle, &mut stat_buf) })?;
        if (res as i64) < 0 {
            trace!(target: "smbc", "statvfs failed");
        }
        Ok(stat_buf)
    }*/

    ///
    /// readdir
    /// Get a single directory entry.
    /// @return          SmbcDirEntry of next directory else
    ///                  error occurs or end-of-directory is reached:
    ///                  - EBADF Invalid directory handle
    ///                  - EINVAL smbc_init() failed or has not been called
    ///
    pub fn readdir(&mut self) -> io::Result<SmbcDirEntry> {
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let dirent = result_from_ptr_mut(unsafe { (self.readdir_fn)(ptr.0, self.handle) })?;
        trace!(target: "smbc", "readdir function successful!");
        if dirent.is_null() {
            let e = Error::new(ErrorKind::Other, "dirent null");
            return Err(e);
        }
        let mut buff: Vec<i8> = Vec::new();
        let len = unsafe { (*dirent).namelen };
        let ptr = unsafe { (&(*dirent).name) as *const i8 };
        for x in 0..len {
            trace!(target: "smbc", "namelen : {}", len);
            trace!(target: "smbc", "{:?}", unsafe { *ptr.offset(x as isize) });
            buff.push(unsafe { *ptr.offset(x as isize) });
        }
        let name_buff: Vec<u8> = buff.iter().map(|c| *c as u8).collect();
        trace!(target: "smbc", "Cursor name {:?}", name_buff);
        let filename = String::from_utf8_lossy(&name_buff);
        trace!(target: "smbc", "Filename: {:?}", filename);
        let d_type = match SmbcType::from(unsafe { (*dirent).smbc_type }) {
            Ok(ty) => ty,
            Err(e) => {
                return Err(e);
            }
        };
        let comment = unsafe {
            CStr::from_ptr((*dirent).comment)
                .to_string_lossy()
                .into_owned()
        };
        Ok(SmbcDirEntry {
            s_type: d_type,
            comment,
            path: PathBuf::from(filename.into_owned()),
        })
    }

    ///
    /// lseek on directories.
    ///
    /// smbc_lseekdir() may be used in conjunction with smbc_readdir() and
    /// smbc_telldir(). (rewind by smbc_lseekdir(fd, NULL))
    ///
    /// @param offset    The offset (as returned by smbc_telldir). Can be
    ///                  NULL, in which case we will rewind
    ///
    /// @return          Nothing on success;
    ///                  Error:
    ///                  - EBADF dh is not a valid directory handle
    ///                  - ENOTDIR if dh is not a directory
    ///                  - EINVAL offset did not refer to a valid dirent or
    ///             	   smbc_init not called.
    ///
    pub fn lseekdir(&self, offset: i64) -> Result<()> {
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let res = to_result_with_errno(
            unsafe { (self.lseekdir_fn)(ptr.0, self.handle, offset as off_t) },
            EINVAL,
        )?;
        if i64::from(res) < 0 {
            trace!(target: "smbc", "lseekdir failed");
        }
        Ok(())
    }

    ///
    /// Get the current directory offset.
    /// smbc_telldir() may be used in conjunction with smbc_readdir() and
    /// smbc_lseekdir().
    /// @return         The current location in the directory stream or -1
    ///                 if an error occur.  The current location is not
    ///                 an offset. Becuase of the implementation, it is a
    ///                 handle that allows the library to find the entry
    ///                 later.
    ///                 - EBADF dh is not a valid directory handle
    ///                 - EINVAL smbc_init() failed or has not been called
    ///                 - ENOTDIR if dh is not a directory
    ///
    pub fn telldir(&mut self) -> Result<off_t> {
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let res = to_result_with_errno(unsafe { (self.telldir_fn)(ptr.0, self.handle) }, EINVAL)?;
        Ok(res as off_t)
    }
}

///
/// An iterator over an SmbcDirectory
/// When you use opendir to open a directory, you can use this iterator
/// to loop through all files/subdirectories
/// (Or you can just call readdir over an over)
///
impl Iterator for SmbcDirectory {
    type Item = io::Result<SmbcDirEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        trace!(target: "smbc", "Attempting to retrieve readdir function");
        trace!(target: "smbc", "Handle: {:?}", self.handle);
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let dirent = match result_from_ptr_mut(unsafe { (self.readdir_fn)(ptr.0, self.handle) }) {
            Ok(d) => d,
            Err(e) => {
                trace!(target: "smbc", "Error! {:?}", e);
                return None;
            }
        };
        trace!(target: "smbc", "Readdir successful!");
        if dirent.is_null() {
            trace!(target: "smbc", "Directory is NULL!!! T");
            // Null means we're done
            return None;
        }
        let mut buff: Vec<i8> = Vec::new();
        let len = unsafe { (*dirent).namelen };
        let ptr = unsafe { (&(*dirent).name) as *const i8 };
        for x in 0..len {
            trace!(target: "smbc", "namelen : {}", len);
            trace!(target: "smbc", "{:?}", unsafe { *ptr.offset(x as isize) });
            buff.push(unsafe { *ptr.offset(x as isize) });
        }

        let name_buff: Vec<u8> = buff.iter().map(|c| *c as u8).collect();
        trace!(target: "smbc", "Cursor name {:?}", name_buff);

        let filename = percent_decode(&name_buff).decode_utf8_lossy();
        trace!(target: "smbc", "Filename: {:?}", filename);

        let s_type = match unsafe { SmbcType::from((*dirent).smbc_type) } {
            Ok(ty) => ty,
            Err(e) => {
                return Some(Err(e));
            }
        };
        trace!(target: "smbc", "FileType: {:?}", s_type);
        let comment = unsafe {
            CStr::from_ptr((*dirent).comment)
                .to_string_lossy()
                .into_owned()
        };
        trace!(target: "smbc", "Comment: {:?}", comment);
        Some(Ok(SmbcDirEntry {
            s_type,
            comment,
            path: PathBuf::from(filename.into_owned()),
        }))
    }
}

///
/// Seek trait for SmbcDirectory
/// Granted, you COULD just use lseek and telldir, but
/// in case you need to have the trait...
///
impl Seek for SmbcDirectory {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        trace!(target: "smbc", "seeking file {:?}", pos);
        let ptr = match self.smbc.lock() {
            Ok(p) => p,
            Err(e) => {
                error!("Poisoned mutex {:?}", e);
                panic!("POISONED MUTEX {:?}!!!!", e)
            }
        };
        let (_, off) = match pos {
            SeekFrom::Start(p) => (SEEK_SET, p as off_t),
            SeekFrom::End(p) => (SEEK_END, p as off_t),
            SeekFrom::Current(p) => (SEEK_CUR, p as off_t),
        };
        let res = to_result_with_errno(
            unsafe { (self.lseekdir_fn)(ptr.0, self.handle, off as off_t) },
            EINVAL,
        )?;
        Ok(res as u64)
    }
}

pub fn num_hours(timestamp: timeval) -> i64 {
    num_seconds(timestamp) / 3600
}

pub fn num_minutes(timestamp: timeval) -> i64 {
    num_seconds(timestamp) / 60
}

pub fn num_seconds(timestamp: timeval) -> i64 {
    if timestamp.tv_sec < 0 && timestamp.tv_usec > 0 {
        (timestamp.tv_sec + 1) as i64
    } else {
        timestamp.tv_sec as i64
    }
}

pub fn num_milliseconds(timestamp: timeval) -> i64 {
    num_microseconds(timestamp) / 1000
}

pub fn num_microseconds(timestamp: timeval) -> i64 {
    let secs = num_seconds(timestamp) * 1_000_000;
    let usecs = micros_mod_sec(timestamp);
    secs + usecs as i64
}

fn micros_mod_sec(timestamp: timeval) -> __suseconds_t {
    if timestamp.tv_sec < 0 && timestamp.tv_usec > 0 {
        //MICROS PER SECOND = 1,000,000
        timestamp.tv_usec - 1_000_000 as __suseconds_t
    } else {
        timestamp.tv_usec
    }
}

pub fn stat_hours(timestamp: timespec) -> i64 {
    stat_seconds(timestamp) / 3600
}

pub fn stat_minutes(timestamp: timespec) -> i64 {
    stat_seconds(timestamp) / 60
}

pub fn stat_seconds(timestamp: timespec) -> i64 {
    if timestamp.tv_sec < 0 && timestamp.tv_nsec > 0 {
        (timestamp.tv_sec + 1) as i64
    } else {
        timestamp.tv_sec as i64
    }
}

pub fn stat_milliseconds(timestamp: timespec) -> i64 {
    stat_microseconds(timestamp) / 1000
}

pub fn stat_microseconds(timestamp: timespec) -> i64 {
    let secs = stat_seconds(timestamp) * 1_000_000;
    let usecs = stat_micros_mod_sec(timestamp);
    secs + usecs as i64
}

fn stat_micros_mod_sec(timestamp: timespec) -> __syscall_slong_t {
    if timestamp.tv_sec < 0 && timestamp.tv_nsec > 0 {
        //MICROS PER SECOND = 1,000,000
        timestamp.tv_nsec - 1_000_000 as __syscall_slong_t
    } else {
        timestamp.tv_nsec
    }
}

pub fn print_timeval_secs(timestamp: timeval) {
    let time = num_seconds(timestamp);
    let naive_datetime = NaiveDateTime::from_timestamp(time, 0);
    let datetime: DateTime<Utc> = DateTime::from_utc(naive_datetime, Utc);
    println!("{:?}", datetime);
}

pub fn print_timespec_secs(timestamp: timespec) {
    let time = stat_seconds(timestamp);
    let naive_datetime = NaiveDateTime::from_timestamp(time, 0);
    let datetime: DateTime<Utc> = DateTime::from_utc(naive_datetime, Utc);
    println!("{:?}", datetime);
}
