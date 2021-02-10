// Copyright 2019 Google LLC
// 
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
// 
//     https://www.apache.org/licenses/LICENSE-2.0
// 
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.


#![no_main]
#![no_std]

extern crate libtock_panic_debug;

use libtock_runtime::{set_main, stack_size, TockSyscalls};

set_main!{main}
// Hack: ask for 3 kB of stack so the runtime init doesn't move the process
// break below the stack pointer before init. The hack can be removed when the
// kernel's memory init is changed to the 2.0 semantics.
stack_size!{3*1024}

// Note: this currently calls into UintPrinter, not LowLevelDebug. When Tock 1.5
// is released, we should replace UintPrinter with LowLevelDebug in golf2, at
// which point this app will work correctly.
fn main() -> ! {
    // LowLevelDebug: App 0x0 prints 0x123
    libtock_low_level_debug::LowLevelDebug::<TockSyscalls>::print1(0x123);

    // LowLevelDebug: App 0x0 prints 0x456 0x789
    libtock_low_level_debug::LowLevelDebug::<TockSyscalls>::print2(0x456, 0x789);

    // Print a series of messages quickly to overfill the queue and demonstrate
    // the message drop behavior.
    for _ in 0..10 {
        libtock_low_level_debug::LowLevelDebug::<TockSyscalls>::print1(0x1);
        libtock_low_level_debug::LowLevelDebug::<TockSyscalls>::print2(0x2, 0x3);
    }

    // Wait for the above to print then output a few more messages.
    // TODO: Sleep

    // LowLevelDebug: App 0x0 prints 0xA
    libtock_low_level_debug::LowLevelDebug::<TockSyscalls>::print1(0xA);

    // LowLevelDebug: App 0x0 prints 0xB 0xC
    libtock_low_level_debug::LowLevelDebug::<TockSyscalls>::print2(0xB, 0xC);

    // LowLevelDebug: App 0x0 status code 0x1
    panic!()
}
