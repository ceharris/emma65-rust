# 6522 VIA Emulation Edge Cases

A reference collection of documented timing quirks and hardware behaviors for the
MOS/Rockwell/WDC 6522 Versatile Interface Adapter, gathered from 6502.org forum
threads, tutorials, and related community sources. Intended as background material
for writing or reviewing a 6522 VIA emulator core (registers, timers, shift register,
interrupts, port latching).

No single authoritative test ROM exists for the VIA (unlike Klaus Dormann's 6502
suite), so this doc is meant to be turned into hand-written test cases rather than
run as a ready-made image.

---

## 1. Timer 1 (T1)

**Latch vs. counter split.** T1 has two 16-bit-wide storage locations: the latches
(T1LL/T1LH) and the live down-counter. Writing the low byte only updates T1LL.
Writing the high byte (T1CH) does three things at once: stores the byte in T1LH,
copies T1LL into the low counter byte, and reloads/starts the 16-bit counter —
this is the actual trigger that starts a countdown. Writing only the low latch has
no effect on the running counter.

**Interrupt-clear asymmetry.** Reading the *low* counter byte (T1CL) clears the T1
interrupt flag in the IFR; reading the *high* byte does not. Writing T1CH also
clears the flag (in addition to starting the timer). This means a `BIT T1CL` (or
`LDA T1CL`) is the idiomatic way to acknowledge a T1 interrupt in an ISR without
disturbing the interrupt-enable state.

**Timeout latency.** When the counter reaches zero, the interrupt flag is set and
counting effectively stops (or reloads, in free-run mode) after what's commonly
described as an extra 1.5 clock cycles beyond the programmed count — i.e. loading
N into the counter produces a timeout after N+1 (some write-ups say N+2, see
below) full clocks, not N. Multiple independent sources describe this "off by
one/two" behavior, so it's worth testing precisely against your reference rather
than trusting a single value.

**The "N+2" convention for periodic interrupts.** Community code for setting up a
periodic jiffy-style interrupt consistently subtracts 2 from the desired cycle
count before loading the latches, e.g. for a 1 ms tick at 1 MHz the loaded value is
998 ($3E6), not 1000 — the reasoning given is that automatic latch→counter reload
in free-run mode itself consumes 2 cycles, separate from IRQ service overhead. Any
correct emulator must reproduce this "N+2" relationship between latch value and
real-world period, not just decrement-to-zero-then-fire.

**One-shot vs. free-run (ACR bit 6).** In one-shot mode, T1 counts down once,
sets the interrupt flag, and stops (holding at $FFFF, still decrementing invisibly
in some descriptions — verify against your reference). In free-run/continuous
mode, on reaching zero the counter automatically reloads from the latches and
keeps counting indefinitely without further CPU intervention, generating repeated
interrupts.

**PB7 square-wave / one-shot pulse output (ACR bit 7).** If PB7 is enabled as a T1
output (requires DDRB bit 7 set AND ACR bit 7 set), one-shot mode produces a single
low pulse on PB7 for the duration of the count; free-run mode toggles PB7 on every
underflow, producing a continuous square wave whose period is fixed by the T1
latch value. This is a commonly-missed detail — PB7 toggling should only happen
when both the DDR and ACR conditions are met simultaneously.

**Re-arming during a running count.** T1LL (the low latch) can be rewritten at any
time while a count is in progress without disturbing the current countdown; the
new value only takes effect on the next reload (continuous mode) or the next
explicit T1CH write (one-shot mode).

---

## 2. Timer 2 (T2)

**Two distinct modes (ACR bit 5).** T2 has only one control bit (unlike T1's two),
selecting between:
- **One-shot/interval mode:** behaves like a simplified one-shot T1 — write low
  latch, then write T2CH to load the counter and start it; times out once and sets
  the interrupt flag. No automatic reload.
- **Pulse-counting mode:** T2 counts negative-going pulses on PB6 instead of Ø2
  clock pulses. The low byte written becomes the initial count; each qualifying
  edge on PB6 decrements it. This turns T2 into an event counter rather than a
  timer.

**Edge polarity on PB6.** Multiple sources describe T2 pulse-counting as
triggering on the falling edge of PB6, matching "negative pulses" language in the
datasheet — worth testing explicitly, since some emulator ports get this edge
backward.

**No high-byte interrupt clear.** As with T1, reading T2CL clears the T2 interrupt
flag; reading T2CH does not.

---

## 3. Shift Register (SR)

The SR has 8 modes selected by ACR bits 4:2, split into disabled, three "shift in"
modes, and four "shift out" modes, distinguished by clock source (Ø2, T2, or
external CB1) and whether CB1 is an input or output.

**Trigger condition.** In T2-controlled and Ø2-controlled modes, shifting starts on
a read or write of the SR register. If the SR interrupt flag is already set at
that moment, shifting starts immediately; otherwise the first shift is deferred
until the next T2 timeout (in T2-controlled modes).

**8 vs. 9 pulse hardware bug (mode 6 / free-running Ø2 shift-out).** A well-known
hardware errata on original MOS-fabricated 6522 parts: shift-out under Ø2 control
can emit one extra (9th) clock pulse on CB1 before the intended 8 data pulses,
observed on an oscilloscope by multiple hobbyists. Reports indicate Rockwell- and
Synertek-sourced parts do **not** exhibit this bug, only original MOS parts do.
Separately, at least one investigation traced an *apparent* 9-pulse symptom to a
software artifact — using 6502 `STA (zp),Y` indirect-indexed addressing performs a
spurious dummy read before the real write, which can itself trigger an extra shift
if the SR is accessed indirectly; switching to absolute addressing resolved it in
that case. When testing this, be sure you're isolating a genuine hardware/emulation
discrepancy from a CPU-side dummy-read artifact.

**Data order.** In shift-out mode 2 (Ø2 control), data shifts out starting from bit
0, into progressively higher bits, on the trailing edge of each clock pulse — i.e.
LSB-first shifting internally, which is the reverse of what protocols like PS/2
(also LSB-first, but with different framing) or SPI (commonly MSB-first) expect;
mismatched bit-order assumptions are a frequent source of confusion when
interfacing real code to the SR.

**Mode 3 (shift-in under CB1 control) is a free-running pulse counter.** Unlike
the shift-out modes, mode 3 does not stop shifting after 8 bits — it just
interrupts every 8th pulse and keeps counting; reading or writing SR resets the
count and clears the flag, but does not stop the clocking. Emulators that treat
this like a one-shot "stop after 8" mode will diverge from real hardware.

**Data validity window.** In external-clock shift-in mode, input data must be
stable through the entire clock cycle following the active edge of the CB1 pulse
— shifting doesn't happen instantaneously on the edge itself but during the
following clock cycle. This matters for cycle-accurate emulation of externally
clocked transfers.

**SPI polarity mismatch.** The VIA's SR always shifts data out on one particular
clock transition (effectively SPI mode 3 timing), and this cannot be reversed in
software — if you're modeling a system that bit-bangs SPI through the VIA and
expects mode 0, the emulator needs to reflect that the VIA can't produce it
natively.

---

## 4. Interrupt Flag Register (IFR) / Interrupt Enable Register (IER)

**Bit 7 is a derived "OR" flag.** IFR bit 7 (IRQ) is automatically set whenever
any *enabled* individual flag (bits 0–6) is set, and automatically clears when all
enabled flags have been cleared — it is not an independent flag you clear
directly the same way as the others.

**Masking before branching.** Because a flag bit in IFR can be set even when its
corresponding IER enable bit is off (e.g. SR completing a shift you're not using
interrupts for), correct polling code (and correct emulator consumers of the
register) should AND IFR with IER before deciding which device asserted the
interrupt, rather than testing IFR alone.

**Per-source clear conditions differ.** Each IFR bit clears via a different
side-effecting action specific to its peripheral function (e.g., T1: read T1CL or
write T1CH; T2: read T2CL or write T2CH; SR: read or write SR; CA1/CA2/CB1/CB2:
reading or writing the associated port register, *unless* the "independent
interrupt" configuration in PCR is set for CA2/CB2, in which case the normal
port-read side effect does not clear that bit). An emulator needs a per-bit clear
rule, not a single generic "any register access clears everything" shortcut.

---

## 5. Port A/B Input Latching and Handshake Lines

**Input latching is opt-in per port.** ACR bits 0 (PA) and 1 (PB) enable input
latching independently for each port. When enabled, the input register captures
the pin state at the active edge of the corresponding CA1/CB1 control line rather
than combinationally reflecting the pin in real time; when disabled, reads of
ORA/ORB reflect the live pin state for bits configured as inputs.

**CA1/CA2/CB1/CB2 edge polarity is independently configurable** via PCR, and
getting the CA2/CB2 mode field wrong (input-negative-edge vs input-positive-edge
vs handshake output vs pulse output vs manual output) is one of the more common
sources of subtle emulator bugs, since each of the 8 PCR-selected behaviors has
different side effects on the associated interrupt flag and, for output modes,
different pulse-width/duration behavior relative to port register accesses.

**Handshake / pulse output timing on CA2/CB2.** In handshake mode, CA2/CB2 goes
low on a read (or write, depending on port) of the associated data register and
returns high on the next active edge of CA1/CB1. In pulse mode, the line pulses
low for exactly one Ø2 cycle following the register access, then returns high
automatically without waiting for an external edge — the one-cycle pulse width is
a specific, testable timing detail rather than an arbitrary duration.

---

## 6. Reset Behavior

Community write-ups note that asserting RES clears most control/data registers
and interrupt flags, but there are open questions/reports about whether it fully
and immediately clears the IRQ output line, and it does not affect a shift already
in progress or reload the timers to a defined running state — reset primarily
puts the chip into a known idle/disabled configuration rather than actively
stopping every internal counter. Treat this as an area to verify against your
specific reference (real chip trace, or a trusted emulator) rather than assuming
a single universal reset behavior, since datasheet wording here is thinner than
for the timers and shift register.

---

## 7. Suggested Test Case Buckets

For turning the above into actual test vectors:

1. T1 one-shot: load latch, verify exact cycle count to interrupt flag set, verify
   PB7 pulse shape if enabled.
2. T1 free-run: verify auto-reload timing (N+2 convention) and repeated PB7 toggle
   period.
3. T1/T2 interrupt-clear asymmetry: read high byte only (flag should remain set),
   then read low byte (flag should clear).
4. T2 one-shot vs. pulse-counting mode, including PB6 edge polarity.
5. SR mode-by-mode: trigger conditions (immediate vs. deferred-to-T2-timeout), bit
   order, stop-after-8 vs. free-running (mode 3) behavior, exact CB1 pulse count
   (watch for the 8-vs-9 pulse case) and mode 2 data valid vs the trailing edge.
6. IFR/IER: verify bit 7 derivation from masked bits, verify each bit's specific
   clear condition, verify PCR "independent interrupt" mode suppresses the normal
   port-access clear for CA2/CB2.
7. Port latching on/off via ACR, independently per port.
8. PCR-selected CA2/CB2 modes: edge-sensitive input, handshake output, pulse
   output (exact one-cycle width), manual output.
9. Reset: verify register/flag initial state; explicitly test what happens to an
   in-flight shift or timer count across reset if your reference model defines it.

---

## Sources

- 6502.org Tutorials — *Investigating Interrupts* (T1 setup, IFR/IER masking
  idiom, T1CL-clears-interrupt idiom): https://6502.org/tutorials/interrupts.html
- 6502.org Source — *One-Shot 6522 Timer Examples* (T1LL/T1CH sequencing):
  http://6502.org/source/io/6522timr.htm
- forum.6502.org — *T1 Timer system ticker* (N+2 latch convention, free-run
  reload cycles): http://forum.6502.org/viewtopic.php?f=2&t=6872
- forum.6502.org — *6522 VIA partial shifts?* (SR mode 3 trigger/edge discussion,
  bit-order confusion vs. PS/2): http://forum.6502.org/viewtopic.php?t=7083
- stardot.org.uk — *via shift register bug* (8-vs-9 pulse hardware errata,
  MOS vs. Rockwell/Synertek parts, dummy-read artifact via indirect addressing):
  https://stardot.org.uk/forums/viewtopic.php?t=21848
- members.tripod.com (Frank Kontros transcription) — *6522 VIA: Shift Register
  Operation* (all 8 SR mode descriptions): https://members.tripod.com/frank_kontros/6522/shiftreg.htm
- ElectronicAdventures blog — *6522 Shift Register — Shift in under PH02 clock*
  (mode 2 walk-through): http://electronicsadventures.blogspot.com/2020/07/6522-shift-register-shift-in-under-ph02.html
- 6502.org/users/andre — *CS/A65 SPI Interface* (VIA SR vs. SPI clock-phase
  mismatch): http://www.6502.org/users/andre/csa/spi/index.html
- Jeff Tranter's Blog — *6522 VIA Experiment #5* (T1 periodic interrupt example,
  BIT T1CL idiom): http://jefftranter.blogspot.com/2012/03/6522-via-experiment-5.html
- robin-6502-project blog — *6522 VIA Timer 1 One-Shot* (N+1.5 cycle timeout
  detail): https://robin-6502-project.blogspot.com/2020/06/6522-via-timer-1-one-shot.html
- robin-6502-project blog — *6522 VIA Shift Register & 7 Segment Display*
  (practical shift-out usage): https://robin-6502-project.blogspot.com/2020/05/6522-via-shift-register-7-segment-display.html
- pico-6502 (jfoucher) `6522.h` header comments, referencing forum.6502.org
  discussion of timer zero-crossing behavior: https://github.com/jfoucher/pico-6502/blob/master/6522.h

Note: a couple of claims above (RES line's effect on an in-flight IRQ, and the
precise +1/+1.5/+2 cycle framing across different write-ups) come from secondary
summaries rather than a single canonical primary source and should be cross-
checked against the original MOS/Rockwell/WDC datasheet timing diagrams and, if
possible, real hardware before being hardened into test assertions.

---

## 8. Implementation Assessment (emma65 `Via6522`)

Cross-referencing the above against `src/emulator/device/via6522.rs` as of
commit `1da6faf`.

### Well-covered

- **T1 latch/counter split** — writing T1CH is the only trigger that starts the
  counter; T1LL writes do not disturb a running count.
- **T1 interrupt-clear asymmetry** — reading T1CL clears IRQ_T1; reading T1CH
  does not; writing T1CH does.
- **T1 one-shot vs. free-run** — one-shot stops at underflow; free-run reloads
  from the latch.
- **T1 PB7 toggle** — ACR bit 7 controls it; ACR takes priority over DDRB for
  PB7 (consistent with the issue #99 fix).
- **T2 timed vs. pulse-count** — ACR bit 5 selects mode; PB6 falling edge
  decrements in pulse-count mode; timed mode ignores PB6.
- **T2 interrupt-clear asymmetry** — reading T2CL clears IRQ_T2; T2CH does not.
- **IFR bit 7 derivation** — computed as `IFR & IER != 0`; not independently
  clearable.
- **Per-source IFR clear rules** — each bit clears via its own specific action;
  PCR "independent interrupt" CA2/CB2 modes suppress the port-access clear.
- **PA/PB input latching** — independently enabled per port, captures on CA1/CB1
  active edge; both live-input and latched-input paths tested.
- **CA2/CB2 PCR modes** — all 8 modes (input negative/positive edge, handshake
  output, pulse output, manual low/high) implemented and tested, including
  correct independent-interrupt suppression.

### Gaps

**T1 timeout latency (N+2 convention).** The implementation fires IRQ_T1 when
the counter reaches zero, meaning a count of N produces a timeout after exactly
N ticks. Real hardware fires after approximately N+1.5 cycles; community code
consistently subtracts 2 from the desired count to compensate. Programs using
that convention will observe their timers firing 2 cycles early. This is the
highest-priority correctness gap for running real-world 6522 driver code, though
the precise offset should be verified against a reference before hardening it
into the emulator.

**SR mode 3 (IN_EXT) should free-run after 8 bits.** `sr_update()` sets IRQ_SR
and zeroes `sr_count` after 8 external clocks, treating it as a one-shot. Per
section 3 above, shift-in under CB1 control does not stop — it asserts IRQ every
8th pulse and keeps counting; only a read or write of the SR register resets the
count. This diverges from real hardware behavior and is a known implementation
gap.

**SR trigger-condition deferral in T2-controlled modes.** In T2/Ø2-controlled
modes the first shift should be deferred to the next T2 timeout unless IRQ_SR is
already set at the time of the SR access. `sr_start()` always begins immediately,
which could cause a first-byte timing discrepancy.

### Minor notes

- The `PortStateChange { port: 'A' }` handler sets IRQ_CA1 on any port A data
  change, which works for the intended virtual-peripheral protocol (data + strobe
  bundled in one message) but diverges from the real hardware model where CA1 is
  an independent pin.
- Pulse output width for CA2/CB2 is instantaneous (two consecutive messages in
  one call) rather than exactly one Ø2 cycle. Correct for virtual peripherals
  that observe the message stream; would need a cycle-accurate model to test the
  one-cycle pulse width from section 5.
