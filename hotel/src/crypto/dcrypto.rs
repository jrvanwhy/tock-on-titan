#![allow(unused_variables)]
#![allow(dead_code)]

//! Software interface to the dcrypto peripheral of the Hotel chip
//! for the Tock operating system.
//!
//! dcrypto is a processor designed to offload the SC300 CPU and
//! accelerate cryptographic algorithms. The primary applications are
//! public key cryptography algorithms such as Elliptic Curve (ECC)
//! and RSA , both over over GF(P) prime finite fields. There is no
//! specific support for accelerated processing over GF(2^m) binary
//! extension fields. dcrypto offers a number of features to defend
//! against side channel analysis (SCA) and fault injection attacks.
//!
//! The engine is fully programmable and thus offers the flexibility
//! to support various algorithms and implementation alternatives. For
//! example, the ECC scalar point multiplication algorithm may be
//! modified in a number of ways in order to reduce secret data
//! leakage on side channels. It is easy to support different curve
//! parameters and prime field moduli. Cryptographic hash algorithms
//! such as SHA512 have also been implemented on dcrypto and run
//! efficiently.
//!
//! The dcrypto processor has a native data wordlength of 256 bits and
//! is optimized for supporting ECC algorithms using prime moduli of
//! size 256 bits or smaller. There are also features in the
//! instruction set which enable support for algorithms using wider
//! operands such as RSA-2048.
//!
//! dcrypto has a custom instruction set and 3 memory banks:
//!    - drom: data read-only memory for constants
//!    - dmem: data memory for input/output (readable/writeable from software)
//!    - imem: instruction memory
//!
//! The standard use case is to load input data into dmem, load instructions
//! into imem, then tell the peripheral to execute an instruction that jumps
//! to the first instruction of the program in imem.
    
use core::cell::Cell;
use core::mem;
use kernel::common::take_cell::TakeCell;
use kernel::common::volatile_cell::VolatileCell;
use kernel::returncode::ReturnCode;

use pmu::{Clock, PeripheralClock, PeripheralClock0, reset_dcrypto};


// NOTE! The manual says this is address 0x40440000, but the Cr50 reference
// code uses 0x40420000 and the system memory map says 0x40420000.
const DCRYPTO_BASE_ADDR: u32 = 0x40420000;
const DCRYPTO_BASE: *mut Registers = DCRYPTO_BASE_ADDR as *mut Registers;

pub static mut DCRYPTO: DcryptoEngine<'static> = unsafe {DcryptoEngine::new(DCRYPTO_BASE) };


const DROM_OFFSET: u32 = 0x2000;
const DROM_SIZE: usize = 1024 * 4;
const DMEM_OFFSET: u32 = 0x4000;
const DMEM_SIZE: usize = 1024 * 4;
const IMEM_OFFSET: u32 = 0x8000;
const IMEM_SIZE: usize = 1024 * 4;

const RAND_STALL_EN: u32 = 0x1;
const RAND_STALL_EN_MASK: u32 = !RAND_STALL_EN;
const RAND_STALL_FREQ_50: u32 = (3 << 1);
const RAND_STALL_FREQ_1: u32 = (3 << 1);
const RAND_STALL_FREQ_2: u32 = (3 << 1);
const RAND_STALL_FREQ_6: u32 = (3 << 1);
const RAND_STALL_FREQ_MASK: u32 = !(0x3 << 1);

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum State {
    Uninitialized,
    Halt,              // PGM_HALT
    Starting,          // Have sent command but no interrupt yet
    Running,           // PGM_RUN
    Break,             // PGM_BRK
    Wiping,            // WIPE_SEC
}

#[derive(Debug, PartialEq)]
pub enum ProgramFault {
    Break,           // Breakpoint reached
    DataAccess,      // Data pointer overflow
    LoopOverflow,    // Loop nesting too deep
    LoopUnderflow,   // Popped when loop depth was 0
    ModOperandRange, // Mod operand out of range
    StackOverflow,   // 
    Fault,           // ?
    Trap,            // Invalid instruction
    Unknown, 
}

impl From<ProgramFault> for usize {
    fn from(original: ProgramFault) -> usize {
        match original {
            ProgramFault::Break           => 7,
            ProgramFault::DataAccess      => 5,
            ProgramFault::LoopOverflow    => 3,
            ProgramFault::LoopUnderflow   => 4,
            ProgramFault::ModOperandRange => 11,
            ProgramFault::StackOverflow   => 2,
            ProgramFault::Fault           => 10,
            ProgramFault::Trap            => 8,
            ProgramFault::Unknown         => 12,
        }
    }
}


#[derive(Debug, Copy, Clone)]
enum InterruptFlag {
    CommandReceive       = 1 << 0,
    CommandDone          = 1 << 1,
    PCStackOverflow      = 1 << 2,
    LoopStackOverflow    = 1 << 3,
    LoopStackUnderflow   = 1 << 4,
    DMemPointersOverflow = 1 << 5,
    DrfPointersOverflow  = 1 << 6,
    Break                = 1 << 7,
    Trap                 = 1 << 8,
    DoneWipeSecrets      = 1 << 9,
    ProgramFault         = 1 << 10,
    OperandOutofRange    = 1 << 11,
}

/// Trait that a module using dcrypto implements for callbacks on operations.
pub trait DcryptoClient<'a> {
    /// Called when an execution completes (Dcrypto engine transitions
    /// from the Running to the Halt state). If error is SUCCESS, the
    /// engine is now in the Halt state and the fault argument is meaningless.
    /// If error is not SUCCESS, fault contains the underlying dcrypto
    /// error.  
    fn execution_complete(&self, error: ReturnCode, fault: ProgramFault);
    
    /// Called when a reset completes. If error is SUCCESS, the engine
    /// is now in the Halt state. If error is not SUCCESS, the state is
    /// undefined.
    fn reset_complete(&self, error: ReturnCode);

    /// Called when a secret wipe completes. If error is SUCCESS, the
    /// engine is now in the Halt state. If error is not SUCCESS, the state
    /// is undefined.
    fn secret_wipe_complete(&self, error: ReturnCode);
}

/// Interface to dcrypto peripheral.
pub trait Dcrypto<'a> {

    /// Set the client to receive callbacks from the engine.
    fn set_client(&self, client: &'a DcryptoClient<'a>);
    
    /// Read the Dcrypto dmem. length is the number of bytes: it must
    /// be <= data.len. Offset is the offset at which to
    /// read.
    fn read_data(&self, data: &mut [u8], offset: u32, length: u32) -> ReturnCode;
    
    /// Write to the Dcrypto dmem. length is the number of bytes: it
    /// must be <= data.len. offset is the offset at which
    /// to perform the write.
    fn write_data(&self, data: &[u8], offset: u32, length: u32) -> ReturnCode;

    /// Read the Dcrypto imem. length is the number of bytes and must
    /// be <= data.len. offset is the offset at which to
    /// read. 
    fn read_instructions(&self, data: &mut [u8], offset: u32, length: u32) -> ReturnCode;
    
    /// Write to the Dcrypto imem. length is the number of bytes and
    /// must be <= data.len. offset is the offset at which
    /// to perform the write.
    fn write_instructions(&self, instructions: &[u8], offset: u32, length: u32) -> ReturnCode;
    
    /// Call to an instruction in instruction memory (IMEM).  Note
    /// that the address is an address, not an instruction index: it
    /// should be word aligned. Address should be a valid instruction
    /// address (inbetween 0 and IMEM_SIZE - 4). If this returns
    /// SUCCESS there will be a completion callback.
    fn call_imem(&self, address: u32) -> ReturnCode;
    
    /// Low-level method to execute an instruction. If the
    /// instruction is a call instruction, the `is_call` parameter
    /// should be true; this tells the peripheral that it should wait
    /// for an interrupt and signal a completion event when the
    /// program finishes. If the instruction is not a call
    /// instruction, the `is_call` parameter should be false; this
    /// tells the driver that it can return immediately and there will
    /// not be a completion callback. Therefore the logic is:
    ///   - is_call: true, ReturnCode::SUCCCESS -- callback
    ///   - is_call: true, ReturnCode not SUCCESS -- no callback
    ///   - is_call: false, ReturnCode::SUCCCESS -- no callback
    ///   - is_call: false, ReturnCode not SUCCCESS -- no callback
    fn execute_instruction(&self, instruction: u32, is_call: bool) -> ReturnCode;

    /// Returns the current execution state of the Dcrypto engine.
    /// Note that since Dcrypto is a co-processor this value is
    /// inherently volatile and can change between invocations.
    fn state(&self) -> State;

    /// Reset the Dcrypto engine.
    fn reset(&self) -> ReturnCode;

    /// Wipe all secrets from the Dcrypto engine.
    fn wipe_secrets(&self) -> ReturnCode;    
}

#[repr(C)]
struct Registers {
    pub version: VolatileCell<u32>,      // 0x0000
    pub control: VolatileCell<u32>,
    pub parity: VolatileCell<u32>,
    pub imem_scrub: VolatileCell<u32>,
    pub dmem_scrub: VolatileCell<u32>,   // 0x0010
    pub int_enable: VolatileCell<u32>,
    pub int_state: VolatileCell<u32>,
    pub int_test: VolatileCell<u32>,
    pub host_cmd: VolatileCell<u32>,     // 0x0020
    pub instr: VolatileCell<u32>,
    pub status: VolatileCell<u32>,
    pub aux_cc: VolatileCell<u32>,     
    pub rand_stall: VolatileCell<u32>,   // 0x0030
    pub rand256: VolatileCell<u32>,
    pub imem_parity: VolatileCell<u32>,
    pub dmem_parity: VolatileCell<u32>,
    pub drf_parity: VolatileCell<u32>,   // 0x0040
    pub pgm_lfsr: VolatileCell<u32>,
    pub debug_brk0: VolatileCell<u32>,
    pub debug_brk1: VolatileCell<u32>,
    pub wipe_secrets: VolatileCell<u32>, // 0x0050
}

pub struct DcryptoEngine<'a> {
    registers: *mut Registers,
    client: Cell<Option<&'a DcryptoClient<'a>>>,
    state: Cell<State>,
    drom: TakeCell<'static, [u8; DROM_SIZE]>,
    dmem: TakeCell<'static, [u8; DMEM_SIZE]>,
    imem: TakeCell<'static, [u8; IMEM_SIZE]>
}

impl<'a> DcryptoEngine<'a> {
    const unsafe fn new(registers: *mut Registers) -> DcryptoEngine<'a> {
        DcryptoEngine {
            registers: registers,
            client: Cell::new(None),
            state: Cell::new(State::Uninitialized),
            drom: TakeCell::empty(),
            dmem: TakeCell::empty(),
            imem: TakeCell::empty(),
        }
    }
    
    pub fn initialize(&mut self) -> ReturnCode {
        unsafe {
            self.drom = TakeCell::new(mem::transmute(DCRYPTO_BASE_ADDR + DROM_OFFSET));
            self.dmem = TakeCell::new(mem::transmute(DCRYPTO_BASE_ADDR + DMEM_OFFSET));
            self.imem = TakeCell::new(mem::transmute(DCRYPTO_BASE_ADDR + IMEM_OFFSET));
        }
                
        let registers: &mut Registers = unsafe {mem::transmute(self.registers)};
        
        // Note: this is a re-implementation of the C code for
        // the Cr52 dcrypto runtime -pal
        if self.state.get() != State::Uninitialized {
            ReturnCode::EALREADY
        } else {
            // Enable PMU and reset it
            unsafe {Clock::new(PeripheralClock::Bank0(PeripheralClock0::Crypto0)).enable();}
            reset_dcrypto();

            // Turn off random no-ops
            let mut stall = registers.rand_stall.get();
            stall = stall & RAND_STALL_EN_MASK;
            registers.rand_stall.set(stall);

            // Configure random no-op percentage to 6%
            stall = stall & RAND_STALL_FREQ_MASK;
            stall = stall | RAND_STALL_FREQ_6;
            registers.rand_stall.set(stall);

            // Turn on random no-ops
            stall = stall | RAND_STALL_EN;
            registers.rand_stall.set(stall);

            // Initialize dmem
            self.dmem.map(|mem| {
                for i in 0..DMEM_SIZE {
                    mem[i] = 0xdd;
                }
            });
            // Initialize imem
            self.imem.map(|mem| {
                for i in 0..IMEM_SIZE {
                    mem[i] = 0xdd;
                }
            });

            // Clear then enable all interrupts: the Cr52 implementation
            // does this but also handles interrupts differently, so we
            // selectively enable below. Left here for reference.
            // registers.int_state.set(0xffffffff);
            // registers.int_enable.set(0xffffffff);

            // Clear all interrupts then enable done interrupt
            // Note: implementation currently does not handle start
            // interrupt due to NVIC re-ordering.
            registers.int_state.set(0xffffffff);
            let interrupts =
                InterruptFlag::CommandDone as u32 |
                InterruptFlag::DMemPointersOverflow as u32 |
                InterruptFlag::DrfPointersOverflow as u32 |
                InterruptFlag::LoopStackOverflow as u32 |
                InterruptFlag::LoopStackUnderflow as u32 |
                InterruptFlag::OperandOutofRange as u32 |
                InterruptFlag::PCStackOverflow as u32 |
                InterruptFlag::ProgramFault as u32 |
                InterruptFlag::Trap as u32;
                
                
            registers.int_enable.set(interrupts);
            //InterruptFlag::CommandDone as u32);
            //registers.int_enable.set(InterruptFlag::CommandDone as u32);
            
            // Reset
            registers.control.set(1);
            registers.control.set(0);

            self.state.set(State::Halt);
            ReturnCode::SUCCESS
        }
    }

    pub fn handle_error_interrupt(&self, nvic: u32) {
        let registers: &mut Registers = unsafe {mem::transmute(self.registers)};
        let cause = match nvic {
            1 => ProgramFault::DataAccess,
            3 => ProgramFault::DataAccess,
            6 => ProgramFault::LoopOverflow,
            7 => ProgramFault::LoopUnderflow,
            8 => ProgramFault::ModOperandRange,
            9 => ProgramFault::StackOverflow,
            10 => ProgramFault::Fault,
            11 => ProgramFault::Trap,
            _ => ProgramFault::Unknown,
        };
        println!("DCRYPTO handling {:?} error interrupt.", cause);

        // Clear the corresponding interrupt flag
        let flag = match nvic {
            1 =>  InterruptFlag::DMemPointersOverflow,
            3 =>  InterruptFlag::DrfPointersOverflow,
            6 =>  InterruptFlag::LoopStackOverflow,
            7 =>  InterruptFlag::LoopStackUnderflow,
            8 =>  InterruptFlag::OperandOutofRange,
            9 =>  InterruptFlag::PCStackOverflow,
            10 => InterruptFlag::ProgramFault,
            11 => InterruptFlag::Trap,
            _ => {
                panic!("DCRYPTO engine handled unknown interrupt, NVIC number is {}", nvic);
            },
        };
        
        registers.int_state.set(flag as u32);
        let prior_state = self.state.get();
        self.state.set(State::Break);
        if prior_state == State::Running || prior_state == State::Break {
            println!("DCRYPTO engine had a {:?} error, now in Break state.", flag);
            self.client.get().map(|client| {
                client.execution_complete(ReturnCode::FAIL, cause);
            });
        } else {
            panic!("DCRYPTO engine had a {:?} error but was not running! State is fatally wrong.", cause);
        }

        
    }

    pub fn handle_receive_interrupt(&self) {
        if self.state.get() != State::Starting {
            panic!("DCRYPTO state is wrong; receive interrupt, driver in state {:?}.", self.state.get());
        } else {
            let registers: &mut Registers = unsafe {mem::transmute(self.registers)};
            // Clear interrupt
            registers.int_state.set(InterruptFlag::CommandReceive as u32);
            self.state.set(State::Running);
        }
    }

    pub fn handle_done_interrupt(&self) {
        if self.state.get() != State::Running {
            panic!("DCRYPTO state is fatally wrong; program complete interrupt but driver in state {:?}.", self.state.get());
        } else {
            let registers: &mut Registers = unsafe {mem::transmute(self.registers)};
            // Clear interrupt
            registers.int_state.set(InterruptFlag::CommandDone as u32);

            self.state.set(State::Halt);
            self.client.get().map(|client| {
                client.execution_complete(ReturnCode::SUCCESS, ProgramFault::Unknown);
            });
        }
    }

    pub fn handle_break_interrupt(&self) {
        panic!("DCRYPTO threw a break interrupt but no code should trigger this.");
    }
}

impl<'a> Dcrypto<'a> for DcryptoEngine<'a> {
    fn set_client(&self, client: &'a DcryptoClient<'a>) {
        self.client.set(Some(client));
    }
   
    fn read_data(&self, data: &mut [u8], offset: u32, length: u32) -> ReturnCode {
        if (offset > DMEM_SIZE as u32) ||
            (length > DMEM_SIZE as u32) ||
            (offset + length > DMEM_SIZE as u32) ||
            length > data.len() as u32 {
                return ReturnCode::ESIZE;
            }

        self.dmem.map(|mem| {
            for i in 0..length {
                data[i as usize] = mem[(offset + i) as usize];
            }
        });
        ReturnCode::SUCCESS
    }
    
    fn write_data(&self, data: &[u8], offset: u32, length: u32) -> ReturnCode {
        if (offset > DMEM_SIZE as u32) ||
            (length > DMEM_SIZE as u32) ||
            (offset + length > DMEM_SIZE as u32) ||
            length > data.len() as u32 {
                return ReturnCode::ESIZE;
            }

        if self.state.get() != State::Halt {
            return ReturnCode::EBUSY;
        }
        
        self.dmem.map(|mem| {
            for i in 0..length {
                mem[(offset + i) as usize] = data[i as usize];
            }
        });
        ReturnCode::SUCCESS
    }

    fn read_instructions(&self, instructions: &mut [u8], offset: u32, length: u32) -> ReturnCode {
        if (offset > IMEM_SIZE as u32) ||
            (length > IMEM_SIZE as u32) ||
            (offset + length > IMEM_SIZE as u32) ||
            length > instructions.len() as u32 {
                return ReturnCode::ESIZE;
            }

        self.imem.map(|mem| {
            for i in 0..length {
                instructions[i as usize] = mem[(offset + i) as usize];
            }
        });
        ReturnCode::SUCCESS
    }
    
    fn write_instructions(&self, instructions: &[u8], offset: u32, length: u32) -> ReturnCode {
        if (offset > IMEM_SIZE as u32) ||
            (length > IMEM_SIZE as u32) ||
            (offset + length > IMEM_SIZE as u32) ||
            length > instructions.len() as u32{
                return ReturnCode::ESIZE;
            }

        if self.state.get() != State::Halt {
            return ReturnCode::EBUSY;
        }
        
        self.imem.map(|mem| {
            for i in 0..length {
                mem[(offset + i) as usize] = instructions[i as usize];
            }
        });
        ReturnCode::SUCCESS
    }

    fn call_imem(&self, address: u32) -> ReturnCode {
        if address > (IMEM_SIZE - 4) as u32 {
            return ReturnCode::ESIZE;
        }
        // 0x08000000 is an opcode of 6'h02, which is the call
        // instruction (DCRYPTO reference).
        self.execute_instruction(0x08000000 + address, true)
    }

    fn execute_instruction(&self, instruction: u32, is_call: bool) -> ReturnCode {
        let registers: &mut Registers = unsafe {mem::transmute(self.registers)};
        if self.state.get() != State::Halt {
            return ReturnCode::EBUSY;
        }
        // Clear any outstanding start or done interrupts
        while {
            registers.int_state.set(0xffffffff);
            registers.int_state.get() & 0x3 != 0
        }{}
        
        registers.host_cmd.set(instruction);
        if is_call {
            self.state.set(State::Running);
        }
        ReturnCode::SUCCESS
    }

    fn state(&self) -> State {
        self.state.get()
    }

    fn reset(&self) -> ReturnCode {
        ReturnCode::FAIL
    }

    fn wipe_secrets(&self) -> ReturnCode {
        ReturnCode::FAIL
    }
}
