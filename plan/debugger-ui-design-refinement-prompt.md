1. The memory view should device the ASCII decode into two 8 byte segments 
   with a space in between, congruent with the hexadecimal decode.
2. The Auto-Step mode could use Shift+F5 as the default shortcut; as it is 
   similar to free-running Run/Stop.
3. For the Flag Display, a compact approach is to simply display a sequence of
   eight characters. For each flag bit that is set, the displayed character 
   is the flag name (e.g. `I`, `Z`, `C`). Flag bits that are unset are displayed as
   `-`. The unused flag bit is always displayed as `-`. Example, For example,
   assuming that interrupts are enabled, the decimal mode is not enabled, and
   the last operation performed by the ALU resulted in A=0 with a carry 
   and no overflow, the complete flag display would be `------ZC`. Flags whose
   values were changed by the last instruction executed in step or 
   auto-step mode could use a different color for distinction.
4. When editing registers, the user will likely anticipate that the radix for
   the value is the same as the current display radix, so we should align 
   with that anticipation by default, and allow a prefix to explicitly 
   specify the radix. For decimal radix, which is less frequently used, we 
   could use `.` or `0d` as the explicit prefix.
5. In the watchpoint display, the current value of the expression should 
   be displayed using a selectable radix, with hexadecimal, unsigned 
   decimal, signed decimal, octal, and binary as display choices, and 
   control cycling between the choices (which applies to all displayed 
   watchpoints).
6. The watchpoint view should provide a means to view and change watch 
   variables (assigned via the walrus operator). These variables are scoped to
   the entire set of watchpoint expressions.
7. Input from the terminal (keystrokes) and output to the terminal 
   (display characters) comes exclusively from the emulated console (via a 
   PipeTransport). I/O for emulated ACIA devices will use an external 
   application (e.g. an application such as Minicom running in a GNOME 
   Terminal and connected to the ACIA device via a PTY or Unix socket 
   transport).
8. Make sure the documentation of keyboard shortcut mappings is consistent 
   between the plan sections entitled "Execution Controls" and "Keyboard 
   Shortcuts", and that the default bindings are consistent with VS Code 
   conventions.
9. By default, the theme should automatically follow the system theme; 
   theme choices should therefore be Auto (default), Dark, Light.
   