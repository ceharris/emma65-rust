

                .include "timer.h.s"

                ; Port mappings and constants for the VIA

                VIA_BASE = $FF80
                VIA_T1CL = VIA_BASE + $4
                VIA_T1CH = VIA_BASE + $5
                VIA_ACR = VIA_BASE + $B
                VIA_IFR = VIA_BASE + $D
                VIA_IER = VIA_BASE + $E

                VIA_ACR_T1_CONTINUOUS = $40

                VIA_IER_SET = $80
                VIA_IER_CLEAR = $0
                VIA_IRQ_T1 = $40


                ; Tick period for 100 Hz clock, assuming 1.8432 MHz system clock
                TICK_PERIOD = 18432

                .segment "BSS"

                ; This field is used to store a tick count as a
                ; 16-bit unsigned integer. With a tick pulse of 400 Hz
                ; the counter, it rolls over after ~163 seconds
timer_ticks:
                .res 2

                ; These fields are used to store the components of the
                ; chronometer, in BCD format
chrono_second:                          ; seconds (0..59)
                .res 1
chrono_centisecond:                     ; centiseconds (0..99)
                .res 1

                .segment "CODE"

;-----------------------------------------------------------------------
; timer_start:
; Initializes Timer 1 on the VIA and starts counting
; time at 100 Hz.
;
timer_start:
                stz timer_ticks+0
                stz timer_ticks+1
                stz chrono_second
                stz chrono_centisecond

                ; Configure T1 for continuous interrupts
                lda VIA_ACR
                ora #VIA_ACR_T1_CONTINUOUS
                sta VIA_ACR

                ; Configure VIA to assert IRQ on T1 timeout
                lda #(VIA_IER_SET | VIA_IRQ_T1)
                sta VIA_IER

                ; Configure T1 period to the tick period and start the timer
                lda #<TICK_PERIOD
                sta VIA_T1CL
                lda #>TICK_PERIOD
                sta VIA_T1CH

                rts


;-----------------------------------------------------------------------
; timer_stop:
; Disable VIA Timer 1 continuous mode and disable Timer 1 interrupts.
;
timer_stop:
                ; Disable T1 continuous mode
                lda VIA_ACR
                and #<(~VIA_ACR_T1_CONTINUOUS)
                sta VIA_ACR

                ; Disable T1 interrupts
                lda #(VIA_IER_CLEAR | VIA_IRQ_T1)
                sta VIA_IER

                rts


;-----------------------------------------------------------------------
; timer_isr:
; Interrupt service routine for the timer interrupt.
;
timer_isr:
                pha

                ; Update the uint32 tick counter, propagating the carry
                ; up through more significant bytes as needed
                inc timer_ticks
                bne @chrono
                inc timer_ticks+1
                bne @chrono
                inc timer_ticks+2
                bne @chrono
                inc timer_ticks+3
                bne @chrono

                ; Update the chronometer, propagating carry out from one
                ; field to the next as needed
@chrono:
                sed                     ; fields are BCD

                ; increment centiseconds
                sec
                lda chrono_centisecond
                adc #0
                sta chrono_centisecond
                bcc @done

                ; carry into seconds
                lda chrono_second
                adc #0                  ; carry set from previous add
                sta chrono_second

@done:
                ; Read T1 LSB to clear interrupt status
                lda VIA_T1CL
                pla
                rti

