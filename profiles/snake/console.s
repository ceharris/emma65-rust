                .include "ascii.h.s"
                .include "console.h.s"
                .include "state.h.s"

                .segment "CODE"

;-----------------------------------------------------------------------
; console_getcb:
; Gets a character from the console, looping (blocking) until a character
; is available.
;
; On return:
; A = character read
;
console_getcb:
                lda CONSOLE_IO
                beq console_getcb
                rts


;-----------------------------------------------------------------------
; console_getcp:
; Gets a character from the console, waiting over a short time interval
; for the character to arrive. This routine is used in distinguishing
; escape sequence input from a terminal from an escape key pressed on
; the keyboard by a human actor. The character(s) following the escape
; in an escape sequence are assumed to arrive much faster than a human
; could legitimately press the escape key followed by another key in
; a sequence like manner. An input routine can use this routine after
; reading Escape from console input. If it returns non-zero, the
; previoulsy read Escape can be assumed to be an escape sequence.
;
; On return:
; A = character read or zero if no character available
;
console_getcp:
                phx
                ldx #$20                ; loop counter
@loop:
                lda CONSOLE_IO          ; read console input
                bne @done               ; go if we got a character
                dex
                bne @loop               ; go if still more passes
@done:
                plx
                ora #0                  ; set Z if A==0
                rts


;-----------------------------------------------------------------------
; console_getcw:
; Waits for a specific letter key or Ctrl+C to be pressed on the
; console keyboard.
;
; On return:
; Carry clear -- expected key was pressed
; Carry set -- Ctrl+C was pressed
;
console_getcw:
                sta B                   ; save character we're awaiting
@loop:
                lda CONSOLE_IO          ; read console input
                beq @loop               ; no input yet
                cmp #CTRL_C
                bne @compare            ; not Ctrl+C: check for match
                sec                     ; set carry to signal Ctrl+C
                rts
@compare:
                and #$df                ; flatten character case
                cmp B
                bne @loop               ; go if not our characcter
                clc                     ; got our character, not Ctrl+C
                rts


;-----------------------------------------------------------------------
; console_puts:
; Puts a string to the console.
;
; On entry:
; W = pointer to the null-terminated string
;
console_puts:
                phy
                ldy #0
@loop:
                lda (W),y
                beq @done
                sta CONSOLE_IO
                iny
                bra @loop
@done:
                ply
                rts


;-----------------------------------------------------------------------
; console_putsw:
; Puts a "wide" string to the console -- i.e. a string in which
; every pair of characters will have an intervening space.
;
; On entry:
; W = pointer to the null-terminated string
;
console_putsw:
                phy
                ldy #0
                lda (W),y
                beq @done
@next:
                sta CONSOLE_IO
                iny
                lda (W),y
                beq @done
                lda #' '
                sta CONSOLE_IO
                lda (W),y
                bra @next
@done:
                ply
                rts


;-----------------------------------------------------------------------
; console_putsc:
; Puts a string to the console consisting a repeated character.
;
; On entry:
; A = character to repeat
; X = number of times to repeat character (A)
;
; On return
; B clobbered
;
console_putsc:
                sta B
@next:
                lda B
                sta CONSOLE_IO
                dex
                bne @next
                rts


;-----------------------------------------------------------------------
; console_drain:
; Drains the console input buffer, discarding any pending input.
;
console_drain:
                stz CONSOLE_LATCH
                rts

