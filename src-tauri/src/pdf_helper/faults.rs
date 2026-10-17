//! Test-only fault injection inside the helper (compiled only into the test binary).
//!
//! A fault request makes the helper misbehave the way a hostile or broken helper could: exit, raise an
//! exception, hang, allocate past its cap, reply after a refused allocation, or send a malformed reply.
//! Fail-fast aborts are not injected: they bypass the exception filter and reach Windows Error Reporting,
//! which could write to the system drive.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::RaiseException;
use windows::Win32::System::Threading::{GetCurrentProcess, SetEvent, TerminateProcess};

use super::protocol::{kind, FrameWriter, Malformed, WireReader, PROTOCOL_VERSION};
use super::serve::Reply;

/// A load whose source starts with this ends the helper with [`CRASH_ON_LOAD_EXIT`].
pub(crate) const CRASH_ON_LOAD_MAGIC: &[u8] = b"%SM5-HELPER-TEST-EXIT-ON-LOAD";
pub(crate) const CRASH_ON_LOAD_EXIT: u32 = 7;
const ACCESS_VIOLATION: u32 = 0xc000_0005;
const EXCEPTION_NONCONTINUABLE: u32 = 1;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Fault {
    /// Ends the helper at once with this exit code.
    Exit(u32),
    /// Raises an access violation (handled by the helper's exception filter).
    RaiseAccessViolation,
    /// Never replies.
    Hang,
    /// Allocates and touches this many bytes through the global allocator, then replies.
    AllocateVec(u64),
    /// Asks the system allocator directly for this many bytes, frees them if granted, and replies either way.
    RefusedAllocationThenReply(u64),
    /// Answers the next request with this malformed reply.
    Misreply(Misreply),
    /// Calls `SetEvent` on this handle value and replies with whether it succeeded.
    SetEvent(u64),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Misreply {
    /// A length prefix of `u32::MAX`, then nothing.
    OversizedLength,
    WrongVersion,
    WrongSequence,
    WrongKind,
    TrailingByte,
    /// Half the frame, then the helper exits.
    TruncatedThenExit,
    /// The last four payload bytes removed (a short bitmap).
    DropLastFourBytes,
    /// The last byte set to 0xFF (invalid UTF-8 at the end of a string).
    CorruptLastByte,
    /// The first payload `u32` incremented (a page count or bitmap width that disagrees).
    BumpFirstPayloadU32,
    /// Page sizes with the last page removed and the count reduced to match.
    DropLastPage,
    /// A load reply that reuses handle 1 (the first document's).
    HandleOne,
    /// The correct reply, then unsolicited frames while no request is pending.
    FloodAfter,
}

const MISREPLIES: [Misreply; 12] = [
    Misreply::OversizedLength,
    Misreply::WrongVersion,
    Misreply::WrongSequence,
    Misreply::WrongKind,
    Misreply::TrailingByte,
    Misreply::TruncatedThenExit,
    Misreply::DropLastFourBytes,
    Misreply::CorruptLastByte,
    Misreply::BumpFirstPayloadU32,
    Misreply::DropLastPage,
    Misreply::HandleOne,
    Misreply::FloodAfter,
];

impl Misreply {
    fn code(self) -> u8 {
        MISREPLIES.iter().position(|item| std::mem::discriminant(item) == std::mem::discriminant(&self)).unwrap() as u8
    }
}

impl Fault {
    pub(crate) fn write(self, writer: &mut FrameWriter) {
        match self {
            Self::Exit(code) => {
                writer.u8(0);
                writer.u32(code);
            }
            Self::RaiseAccessViolation => writer.u8(1),
            Self::Hang => writer.u8(2),
            Self::AllocateVec(bytes) => {
                writer.u8(3);
                writer.u64(bytes);
            }
            Self::RefusedAllocationThenReply(bytes) => {
                writer.u8(4);
                writer.u64(bytes);
            }
            Self::Misreply(misreply) => {
                writer.u8(5);
                writer.u8(misreply.code());
            }
            Self::SetEvent(handle) => {
                writer.u8(6);
                writer.u64(handle);
            }
        }
    }

    pub(crate) fn read(reader: &mut WireReader<'_>) -> Result<Self, Malformed> {
        Ok(match reader.u8()? {
            0 => Self::Exit(reader.u32()?),
            1 => Self::RaiseAccessViolation,
            2 => Self::Hang,
            3 => Self::AllocateVec(reader.u64()?),
            4 => Self::RefusedAllocationThenReply(reader.u64()?),
            5 => Self::Misreply(*MISREPLIES.get(reader.u8()? as usize).ok_or_else(|| Malformed::new("unknown misreply"))?),
            6 => Self::SetEvent(reader.u64()?),
            other => return Err(Malformed::new(format!("unknown fault {other}"))),
        })
    }
}

fn exit_now(code: u32) -> ! {
    let _ = unsafe { TerminateProcess(GetCurrentProcess(), code) };
    loop {
        std::thread::park();
    }
}

pub(crate) fn exit_if_crash_on_load(bytes: &[u8]) {
    if bytes.starts_with(CRASH_ON_LOAD_MAGIC) {
        exit_now(CRASH_ON_LOAD_EXIT);
    }
}

pub(crate) fn run(fault: Fault, misreply: &mut Option<Misreply>, sequence: u32) -> Reply {
    let mut out = FrameWriter::new(kind::FAULT, sequence);
    match fault {
        Fault::Exit(code) => exit_now(code),
        Fault::RaiseAccessViolation => {
            unsafe { RaiseException(ACCESS_VIOLATION, EXCEPTION_NONCONTINUABLE, None) };
            out.u8(0);
        }
        Fault::Hang => loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        },
        Fault::AllocateVec(bytes) => {
            let memory = vec![1_u8; usize::try_from(bytes).unwrap()];
            std::hint::black_box(&memory);
            out.u8(1);
        }
        Fault::RefusedAllocationThenReply(bytes) => {
            let layout = Layout::from_size_align(usize::try_from(bytes).unwrap(), 4096).unwrap();
            let pointer = unsafe { System.alloc(layout) };
            if pointer.is_null() {
                out.u8(0);
            } else {
                unsafe { System.dealloc(pointer, layout) };
                out.u8(1);
            }
        }
        Fault::Misreply(next) => {
            *misreply = Some(next);
            out.u8(1);
        }
        Fault::SetEvent(handle) => {
            let set = unsafe { SetEvent(HANDLE(handle as usize as *mut core::ffi::c_void)) }.is_ok();
            out.u8(u8::from(set));
        }
    }
    Ok(out)
}

/// Writes `frame` (a finished, well-formed reply) altered by `misreply`.
pub(crate) fn send_misreply(output: &mut impl Write, misreply: Misreply, mut frame: Vec<u8>) -> bool {
    let set_length = |frame: &mut Vec<u8>| {
        let length = u32::try_from(frame.len() - 4).unwrap();
        frame[..4].copy_from_slice(&length.to_le_bytes());
    };
    match misreply {
        Misreply::OversizedLength => frame = u32::MAX.to_le_bytes().to_vec(),
        Misreply::WrongVersion => frame[4..6].copy_from_slice(&(PROTOCOL_VERSION + 1).to_le_bytes()),
        Misreply::WrongSequence => {
            let sequence = u32::from_le_bytes(frame[8..12].try_into().unwrap()).wrapping_add(1);
            frame[8..12].copy_from_slice(&sequence.to_le_bytes());
        }
        Misreply::WrongKind => frame[6..8].copy_from_slice(&kind::HELLO.to_le_bytes()),
        Misreply::TrailingByte => {
            frame.push(0);
            set_length(&mut frame);
        }
        Misreply::TruncatedThenExit => {
            let half = frame.len() / 2;
            let _ = output.write_all(&frame[..half]).and_then(|()| output.flush());
            exit_now(0);
        }
        Misreply::DropLastFourBytes => {
            frame.truncate(frame.len() - 4);
            set_length(&mut frame);
        }
        Misreply::CorruptLastByte => {
            *frame.last_mut().unwrap() = 0xff;
        }
        Misreply::BumpFirstPayloadU32 => {
            let value = u32::from_le_bytes(frame[12..16].try_into().unwrap()).wrapping_add(1);
            frame[12..16].copy_from_slice(&value.to_le_bytes());
        }
        Misreply::HandleOne => frame[12..16].copy_from_slice(&1_u32.to_le_bytes()),
        Misreply::FloodAfter => {
            if output.write_all(&frame).and_then(|()| output.flush()).is_err() {
                return false;
            }
            let mut stray = FrameWriter::new(kind::PAGE_SIZES, 0);
            stray.raw(&vec![0; 60 * 1024]);
            let stray = stray.finish();
            for _ in 0..64 {
                if output.write_all(&stray).and_then(|()| output.flush()).is_err() {
                    break;
                }
            }
            return true;
        }
        Misreply::DropLastPage => {
            let count = u32::from_le_bytes(frame[12..16].try_into().unwrap()) - 1;
            frame[12..16].copy_from_slice(&count.to_le_bytes());
            frame.truncate(frame.len() - 8);
            set_length(&mut frame);
        }
    }
    output.write_all(&frame).and_then(|()| output.flush()).is_ok()
}
