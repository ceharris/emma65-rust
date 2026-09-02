Project Overview
----------------

This project contains a high-performance and highly-configurable emulator
for the CMOS 65C02 (and related WDC65C02) microprocessor. It includes a
configurable bus that uses the builder pattern to assemble a bus consisting
of any combination of RAM and ROM and to map any of built-in 
device into the address space. Built-in devices include a comprehensive 
implementation of the 6522 VIA, the R6551 ACIA, the MC6850 ACIA. These devices
take advantage of the interrupt support in the emulated CPU, and support a
variety of byte-oriented transports for communication with peripherals
(real or virtual). Also included is a simple poll-based console port 
similar to those used by period microcomputer systems. 

In addition to built-in devices, a public API is provided to facilitate 
custom device implementations. The configuration subsystem for memory and
I/O devices supports flexibility configuration of the built-in devices and
is extensible to custom devices.

The emulator module includes a binary utility that can be used to load and
execute 6502 programs in a manner very similar to real hardward.

Also present in the project is a sophisticated watchpoint compiler and a 
stack-based virtual machine for efficient evaluation of watchpoint expressions 
at 6502 CPU runtime.

Looking Forward
---------------

The next phase of development in this project is the creation of an IDE-like
debugger that will utilize the underlying emulator and watchpoint support.

The debugger will be based on Tauri (version 2) and will utilize the React 
framework to create a component-based UI using platform web view support.

High-level components of the UI will include:
* A memory view that provides a scrolling view pane into the 6502 address 
  space. This view pane will feature a 256-byte (a.k.a. one page) view 
  with hexadecimal and ASCII displays of memory contents. The memory view 
  will utilize the _peek_ function of the `IoTrait` to idempotently read
  both memory and enulated device registers.
* A view displaying disassembly around the current program counter, 
  highlighting the next instruction to be executed, as well as those 
  within a small range of addresses at addresses lower or higher than the 
  current PC. The disassembly view would also provide a convenient means 
  to see and clear simple breakpoints. The disassembly view should include 
  columns for the address, object code, mnemonic, and operands.
* A register view, displaying the 6502 machine registers and a visual 
  breakdown of flags in the program status register (P). This view should 
  provide the ability to easily edit the value of any register, and should
  support the ability to display the value of the registers in signed or 
  unsigned decimal, as well as binary, octal, or hexadecimal radix.
* A stack view, displaying the contents of the stack as pairs of words, 
  near the current stack pointer. It should support even or odd word 
  alignment at the current pointer, and the ability to toggle the radix of 
  display. 
* A virtual terminal for the console port, with full VT220/Xterm
  compatibility.
* A watchpoint view displaying each configured watchpoint with controls 
  to enable/disable any watchpoint, and differentiated display for watchpoints
  that evaluate to true given the current CPU, memory, and local variable
  context.

The UI will provide controls and keyboard shortcuts to step into, step 
over, step return, and to start/stop repeated single-step, or start/stop free
running CPU mode.

Additionally, the UI could provide controls and views allowing the user to
assemble memory and devices in a bus configuration for a subsequent debugging
session. These configurations could be saved and recalled.

Next Steps
----------

I want you to help me plan the overall visual and functional design of the UI,
based on the description of the desired views and capabilities. The goal 
is to produce a specification in sufficient detail to allow subsequent 
work to produce a detailed and structured implementation plan, facilitating
incremental development using a user-story-centric agile approach.

Ask additional questions to further clarify the requirements and my intent.
Please ask questions one at a time, in an interactive manner to help 
facilitate immediate follow-ups as needed.