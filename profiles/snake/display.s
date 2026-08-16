
                .include "ascii.h.s"
                .include "console.h.s"
                .include "delay.h.s"
                .include "display.h.s"
                .include "model.h.s"
                .include "prng.h.s"
                .include "state.h.s"
                .include "timer.h.s"

        .macro UI_PUT_ANSI_CSI
                lda #ESC
                sta CONSOLE_IO
                lda #'['
                sta CONSOLE_IO
        .endmacro

        .macro UI_PUT_ANSI_CUP
                lda #'H'
                sta CONSOLE_IO
        .endmacro

        .macro UI_PUT_ANSI_ED p
                lda #p
                sta CONSOLE_IO
                lda #'J'
                sta CONSOLE_IO
        .endmacro

        .macro UI_PUT_EMPTY_COLOR
                UI_PUT_ANSI_CSI
                lda #'0'
                sta CONSOLE_IO
                lda #'m'
                sta CONSOLE_IO
        .endmacro

        .macro UI_PUT_DUMP_COLOR
                UI_PUT_ANSI_CSI
                lda #'4'
                sta CONSOLE_IO
                lda #'4'
                sta CONSOLE_IO
                lda #'m'
                sta CONSOLE_IO
                UI_PUT_ANSI_CSI
                lda #'3'
                sta CONSOLE_IO
                lda #'7'
                sta CONSOLE_IO
                lda #'m'
                sta CONSOLE_IO
        .endmacro

        .macro UI_PUT_SNAKE_ALT_COLOR
                UI_PUT_ANSI_CSI
                lda #'3'
                sta CONSOLE_IO
                lda #'5'
                sta CONSOLE_IO
                lda #'m'
                sta CONSOLE_IO
        .endmacro

        .macro UI_PUT_SNAKE_COLOR
                UI_PUT_ANSI_CSI
                lda #'9'
                sta CONSOLE_IO
                lda #'3'
                sta CONSOLE_IO
                lda #'m'
                sta CONSOLE_IO
        .endmacro

        .macro UI_PUT_HEAD_COLOR
                UI_PUT_ANSI_CSI
                lda #'9'
                sta CONSOLE_IO
                lda #'2'
                sta CONSOLE_IO
                lda #'m'
                sta CONSOLE_IO
        .endmacro

        .macro UI_PUT_BLOOD_COLOR
                UI_PUT_ANSI_CSI
                lda #'3'
                sta CONSOLE_IO
                lda #'1'
                sta CONSOLE_IO
                lda #'m'
                sta CONSOLE_IO
        .endmacro

        .macro UI_PUT_SNAKE_SEGMENT
                lda #$E2
                sta CONSOLE_IO
                lda #$96
                sta CONSOLE_IO
                lda #$88
                sta CONSOLE_IO
                lda #$E2
                sta CONSOLE_IO
                lda #$96
                sta CONSOLE_IO
                lda #$88
                sta CONSOLE_IO
        .endmacro

        .macro UI_PUT_EMPTY_SEGMENT
                lda #' '
                sta CONSOLE_IO
                lda #' '
                sta CONSOLE_IO
        .endmacro

        .macro UI_HIDE_CURSOR
                UI_PUT_ANSI_CSI
                lda #'?'
                sta CONSOLE_IO
                lda #'2'
                sta CONSOLE_IO
                lda #'5'
                sta CONSOLE_IO
                lda #'l'
                sta CONSOLE_IO
        .endmacro

        .macro UI_SHOW_CURSOR
                UI_PUT_ANSI_CSI
                lda #'?'
                sta CONSOLE_IO
                lda #'2'
                sta CONSOLE_IO
                lda #'5'
                sta CONSOLE_IO
                lda #'h'
                sta CONSOLE_IO
        .endmacro


                .segment "BSS"
cursor_row:
                .res 4
cursor_column:
                .res 4


                .segment "CODE"


;-----------------------------------------------------------------------
; ui_begin:
; Gets the dimensions of the screen by sending an absurd lower right
; corner cursor position, then sending a cursor position request to read
; the actual cursor location.
;
; On return:
; Carry clear: grid_width, grid_height, screen_width, screen_height
; all set according to the screen dimensions
; Carry set: cursor position request failed, error message displayed
;
ui_begin:
                ldy #3
@again:
                ; try to move the cursor to an absurd location
                ldiw _absurd_cup
                jsr console_puts

                ; send cursor position request
                ldiw _cursor_position_request
                jsr console_puts

                DELAY $8000

                ; read actual cursor position
                jsr console_getcp
                cmp #ESC
                beq @got_esc
@retry:
                jsr console_drain
                dey
                bne @again
                bra @error
@got_esc:
                jsr console_getcp
                cmp #'['
                bne @retry

                ; read row position
                ldx #0
@row_loop:
                jsr console_getcb
                cmp #';'
                beq @row_finish
                cmp #'0'
                bcc @error
                cmp #'9'+1
                bcs @error
                sta cursor_row,x
                inx
                bra @row_loop
@row_finish:
                stz cursor_row,x

                ;read column position
                ldx #0
@col_loop:
                jsr console_getcb
                cmp #'R'
                beq @col_finish
                cmp #'0'
                bcc @error
                cmp #'9'+1
                bcs @error
                sta cursor_column,x
                inx
                bra @col_loop
@col_finish:
                stz cursor_column,x

                ; convert the row number string
                ldiw cursor_row
                jsr _acdbin8

                ; save height in screen and grid rows
                sta screen_height
                dec                     ; lose one row for status line
                sta grid_height

                ; convert the column number string
                ldiw cursor_column
                jsr _acdbin8

                ; save width in screen and grid columns
                sta screen_width
                lsr                     ; two screen columns per grid cell
                sta grid_width
                clc
                rts
@error:
                ldiw _setup_error
                jsr console_puts
                sec
                rts


;-----------------------------------------------------------------------
; ui_clear:
; Displays the initial state of the game UI.
;
ui_clear:
                ; clear display
                ldiw _clear_display
                jsr console_puts

                lda #1                  ; status line starts in column 1
                jsr ui_put_status_cup   ; position it
                ldiw _status_line_pre
                jsr console_puts        ; set attributes
                lda #' '
                ldx screen_width
                jsr console_putsc       ; fill status line

                ; display timer label
                lda #TIMER_LABEL_OFFSET ; positive offset from left edge
                jsr ui_put_status_cup
                ldiw _timer_label
                jsr console_puts

                ; display title label
                lda screen_width
                sec
                sbc #TITLE_WIDTH        ; A = screen width less title width
                lsr                     ; divide by two to center
                jsr ui_put_status_cup   ; position it
                ldiw _title_label
                jsr console_putsw       ; print it

                ; display lives label
                lda screen_width
                clc
                adc #<LIVES_LABEL_OFFSET; negative offset from right edge
                jsr ui_put_status_cup   ; position it
                ldiw _lives_label
                jsr console_puts        ; print it

                ; display score label
                lda screen_width
                clc
                adc #<SCORE_LABEL_OFFSET; negative offset from right edge
                jsr ui_put_status_cup   ; position it
                ldiw _score_label
                jsr console_puts        ; print it

                ; display timer
                jsr ui_put_timer

                ; display current score
                jsr ui_put_score

                ; display current life count
                jsr ui_put_life_count

                ; finish the status line
                ldiw _status_line_post
                jsr console_puts

                rts


;-----------------------------------------------------------------------
; ui_exit:
; Prepares the screen for a UI exit, by clearing the screen and
; restoring mode settings to a sane state.
;
ui_exit:
                ldiw _reset_display
                jsr console_puts
                rts


;-----------------------------------------------------------------------
; ui_redraw:
; Completely redraws the UI display at the current state of the game.
;
; On return:
; B clobbered
;
ui_redraw:
                jsr ui_clear
                ldy #0                  ; Y coordinate
@next_row:
                ldx #0                  ; X coordinate
@next_column:
                jsr ui_put_grid_cup
                UI_PUT_EMPTY_COLOR
                jsr model_get_cell
                sta B
                beq @empty
                and #$80
                beq @food
                UI_PUT_SNAKE_COLOR
                UI_PUT_SNAKE_SEGMENT
                bra @until_done
@food:
                lda B
                adc #'0'
                sta CONSOLE_IO
                sta CONSOLE_IO
                bra @until_done
@empty:
                UI_PUT_EMPTY_SEGMENT
@until_done:
                inx                     ; next grid column
                cpx grid_width
                bne @next_column
                iny                     ; next grid row
                cpy grid_height
                bne @next_row
                jsr console_drain
                rts


;-----------------------------------------------------------------------
; ui_update:
; Updates the UI display to match the current state of the game model
;
ui_update:
                lda game_flags
                asl                     ; carry = food change flag
                bcc @check_grow_count
                asl                     ; carry = food waiting flag
                bcs @food_waiting
                ldx food_x
                ldy food_y0
                jsr ui_put_empty_cell
                ldx food_x
                ldy food_y1
                jsr ui_put_empty_cell
                bra @check_grow_count
@food_waiting:
                ldx food_x
                ldy food_y0
                lda (food_addr_0)
                jsr ui_put_food_cell
                ldx food_x
                ldy food_y1
                lda (food_addr_1)
                jsr ui_put_food_cell
@check_grow_count:
                ldx prev_head_x
                ldy prev_head_y
                jsr ui_put_alt_snake_segment
                lda grow_count
                bne @head
                ldx prev_tail_x
                ldy prev_tail_y
                jsr ui_put_empty_cell
@head:
                ldx snake_head_x
                ldy snake_head_y
                jsr ui_put_snake_segment

                lda game_flags
                and #GF_SCORE_CHANGE
                beq @check_timer
                jsr ui_put_score

@check_timer:
                lda game_flags
                and #GF_TIMER_CHANGE
                beq @done
                jsr ui_put_timer

@done:
                ; clear UI change flags
                lda game_flags
                and #<(~GF_UI_CHANGE_BITS)
                sta game_flags
                rts


;-----------------------------------------------------------------------
; ui_life_over:
; Updates the UI to show that a life has ended.
;
ui_life_over:
                ldx #5
@loop:
                phx
                ldx snake_head_x
                ldy snake_head_y
                jsr ui_put_bloody_segment
                DELAY $8000
                jsr ui_put_empty_cell
                DELAY $8000
                plx
                dex
                bne @loop
                rts

;-----------------------------------------------------------------------
; ui_game_over:
; Update UI display when game is over.
;
ui_game_over:
                ; display play again label
                lda #PLAY_AGAIN_OFFSET  ; positive offset from left edge
                jsr ui_put_status_cup   ; position it
                ldiw _play_again_pre
                jsr console_puts        ; set attributes
                ldiw _play_again_label
                jsr console_puts        ; print it
                ldiw _play_again_post
                jsr console_puts        ; reset attributes

                ; display zero life count
                jsr ui_put_life_count

                ; display game over label
                jsr ui_show_game_over

                rts

;-----------------------------------------------------------------------
; ui_play_again:
; Flash GAME OVER while waiting for the user to decide whether to play
; again.
;
ui_play_again:
                inc lives
                lda lives
                cmp #4
                beq @hide_game_over
                cmp #8
                beq @show_game_over
                bra @blink
@hide_game_over:
                jsr ui_hide_game_over
                bra @blink
@show_game_over:
                jsr ui_show_game_over
                stz lives
@blink:
                ldx snake_head_x
                ldy snake_head_y
                lda lives
                lsr
                bcc @empty

                jsr ui_put_bloody_segment
                bra @finish
@empty:
                jsr ui_put_empty_cell
@finish:
                DELAY $8000
                rts


;-----------------------------------------------------------------------
; ui_show_game_over:
; Shows the game over label in the status line.
;
ui_show_game_over:
                lda screen_width
                sec
                sbc #GAME_OVER_WIDTH    ; A = screen width - label width
                lsr                     ; divide by two to center
                jsr ui_put_status_cup   ; position it
                ldiw _game_over_pre
                jsr console_puts        ; set attributes
                ldiw _game_over_label
                jsr console_putsw       ; print it
                ldiw _game_over_post
                jsr console_puts        ; reset attributes
                rts


;-----------------------------------------------------------------------
; ui_hide_game_over:
; Hides the game over label in the status line.
;
ui_hide_game_over:
                lda screen_width
                sec
                sbc #GAME_OVER_WIDTH    ; A = screen width - label width
                lsr                     ; divide by two to center
                jsr ui_put_status_cup   ; position it
                ldiw _game_over_pre
                jsr console_puts        ; set attributes
                ldx #GAME_OVER_WIDTH
                lda #SPC
                jsr console_putsc       ; print over it
                ldiw _game_over_post
                jsr console_puts        ; reset attributes
                rts


;-----------------------------------------------------------------------
; ui_put_bloody_segment:
;
; On entry:
; X = grid X coordinate
; Y = grid Y coordinate
;
ui_put_bloody_segment:
                jsr ui_put_grid_cup
                UI_PUT_BLOOD_COLOR
                UI_PUT_SNAKE_SEGMENT
                rts

;-----------------------------------------------------------------------
; ui_put_snake_segment:
;
; On entry:
; X = grid X coordinate
; Y = grid Y coordinate
;
ui_put_snake_segment:
                jsr ui_put_grid_cup
                UI_PUT_HEAD_COLOR
                UI_PUT_SNAKE_SEGMENT
                rts


;-----------------------------------------------------------------------
; ui_put_alt_snake_segment:
;
; On entry:
; X = grid X coordinate
; Y = grid Y coordinate
;
ui_put_alt_snake_segment:
                jsr ui_put_grid_cup
                lda game_flags
                and #GF_ALT_COLOR
                bne @put_alt_color
                UI_PUT_SNAKE_COLOR
                bra @put_segment
@put_alt_color:
                UI_PUT_SNAKE_ALT_COLOR
@put_segment:
                UI_PUT_SNAKE_SEGMENT
                lda game_flags
                eor #GF_ALT_COLOR
                sta game_flags
                rts


;-----------------------------------------------------------------------
; ui_put_empty_cell:
;
; On entry:
; X = grid X coordinate
; Y = grid Y coordinate
;
ui_put_empty_cell:
                jsr ui_put_grid_cup
                UI_PUT_EMPTY_COLOR
                UI_PUT_EMPTY_SEGMENT
                rts


;-----------------------------------------------------------------------
; ui_put_food_cell:
; Displays a food cell.
;
; On entry:
; A = food value
; X = grid X coordinate
; Y = grid Y coordinate
;
ui_put_food_cell:
                pha
                jsr ui_put_grid_cup
                UI_PUT_EMPTY_COLOR
                pla
                clc
                adc #'0'
                sta CONSOLE_IO
                sta CONSOLE_IO
                rts


;-----------------------------------------------------------------------
; ui_put_life_count:
; Displays the current life count in the status line
;
ui_put_life_count:
                lda screen_width
                clc
                adc #<LIVES_OFFSET      ; negative offset from right edge
                jsr ui_put_status_cup   ; position it
                ldiw _lives_pre
                jsr console_puts        ; set attributes
                lda lives
                clc
                adc #'0'                ; convert to ASCII decimal
                sta CONSOLE_IO          ; print it
                ldiw _lives_post
                jsr console_puts        ; reset attributes
                rts


;-----------------------------------------------------------------------
; ui_put_timer:
; Displays the current timer in the status line.
;
ui_put_timer:
                lda #TIMER_OFFSET       ; positive offset from left edge
                jsr ui_put_status_cup   ; position it
                ldiw _timer_pre
                jsr console_puts        ; set attributes
                lda food_expires
                jsr _pbcd8              ; print timer value
                ldiw _timer_post
                jsr console_puts        ; reset attributes
                rts


;-----------------------------------------------------------------------
; ui_put_score:
; Displays the current score in the status line.
;
ui_put_score:
                lda screen_width
                clc
                adc #<SCORE_OFFSET      ; negative offset from right edge
                jsr ui_put_status_cup   ; position it
                ldiw _score_pre
                jsr console_puts        ; set attributes
                lda score+1
                ldx score
                jsr _pbcd16             ; print score
                ldiw _score_post
                jsr console_puts        ; reset attributes
                rts


;----------------------------------------------------------------------
; ui_dump_grid
; Dump the contents of the grid in hexdecimal.
;
; On return:
; B clobbered
;
ui_dump_grid:
                ; clear display
                ldiw _clear_display
                jsr console_puts
                ldy #0                  ; Y coordinate
@next_row:
                ldx #0                  ; X coordinate
@next_column:
                jsr model_get_cell      ; fetch the cell
                sta B
                bne @non_empty
                UI_PUT_EMPTY_COLOR
                lda #'-'
                sta CONSOLE_IO
                sta CONSOLE_IO
                bra @check_column
@non_empty:
                UI_PUT_DUMP_COLOR
                lda B
                jsr _phex8              ; print cell in hexadecimal
@check_column:
                inx                     ; next column
                cpx grid_width
                bne @next_column        ; go if more columns
@no_carry:
                lda #CR
                sta CONSOLE_IO
                lda #LF
                sta CONSOLE_IO
                iny                     ; next row
                cpy grid_height
                bne @next_row           ; go if more rows

                ldiw _status_line_pre
                jsr console_puts
                ldy #8
@loop:
                ldx #4
                lda #SPC
                jsr console_putsc
                lda #'+'
                sta CONSOLE_IO
                ldx #4
                lda #SPC
                jsr console_putsc
                lda #'|'
                sta CONSOLE_IO
                dey
                bne @loop
                ldiw _status_line_post
                jsr console_puts
                rts


;-----------------------------------------------------------------------
; ui_put_grid_cup:
; Puts an ANSI CUP (cursor position) sequence representing a grid
; coordinate pair into the serial output buffer.
;
; On entry:
; X = grid X coordinate
; Y = grid Y coordinate
;
ui_put_grid_cup:
                UI_PUT_ANSI_CSI

                ; row number is first
                tya                     ; Y coordinate to A
                inc                     ; rows start at 1 not 0
                jsr _bin8bcd            ; convert to BCD in W
                lda W+1
                beq @y_lower            ; not three digits, skip MSB
                jsr _pbcd8n             ; put MSB digits
                lda W
                jsr _pbcd8u             ; put LSB digits
                bra @put_sep
@y_lower:
                lda W
                jsr _pbcd8n             ; put LSB digits
@put_sep:
                ; put separator
                lda #';'
                sta CONSOLE_IO

                ; column number is second
                txa                     ; X coordinate to A
                asl                     ; two screen columns per grid column
                inc                     ; columns start at 1 not 0
                jsr _bin8bcd            ; convert to BCD in W
                lda W+1
                beq @x_lower            ; not three digits, skip MSB
                jsr _pbcd8n             ; put MSB digits
                lda W
                jsr _pbcd8u             ; put LSB digits
                bra @put_cup
@x_lower:
                lda W
                jsr _pbcd8n             ; put LSB digits
@put_cup:
                UI_PUT_ANSI_CUP
                rts


;-----------------------------------------------------------------------
; ui_put_status_cup:
; Positions the cursor at a specified column on the status line.
;
; On entry:
; A = column number (1..screen_width)
;
ui_put_status_cup:
                pha                     ; save column number
                UI_PUT_ANSI_CSI

                ; row number is first
                lda screen_height       ; status is on the last line
                jsr _bin8bcd            ; convert to BCD in W
                lda W+1
                beq @y_lower            ; not three digits, skip MSB
                jsr _pbcd8n             ; put MSB digits
                lda W
                jsr _pbcd8u             ; put LSB digits
                bra @put_sep
@y_lower:
                lda W
                jsr _pbcd8n             ; put LSB digits
@put_sep:
                ; put separator
                lda #';'
                sta CONSOLE_IO

                ; column number is second
                pla
                jsr _bin8bcd            ; convert to BCD in W
                lda W+1
                beq @x_lower            ; not three digits, skip MSB
                jsr _pbcd8n             ; put MSB digits
                lda W
                jsr _pbcd8u             ; put LSB digits
                bra @put_cup
@x_lower:
                lda W
                jsr _pbcd8n             ; put LSB digits
@put_cup:
                UI_PUT_ANSI_CUP
                rts


;-----------------------------------------------------------------------
; _bin8bcd:
; Convert an 8-bit binary value in A to a two- or three-digit BCD
; value.
;
; On entry:
; A = 8 bit value to convert
;
; On return:
; W = BCD digits that correspond to A
; B clobbered
;
_bin8bcd:
                phx

                ; save value to convert
                sta B

                ; zero out the result buffer
                stz W
                stz W+1

                ldx #8                  ; bit counter
                sed
@loop:
                asl B                   ; high order bit of arg to carry
                lda W
                adc W                   ; double LSB + carry
                sta W
                lda W+1
                adc W+1                 ; double MSB + carry
                sta W+1
                dex
                bne @loop

                cld
                plx
                rts


;-----------------------------------------------------------------------
; _acdbin8:
; Converts a zero-terminal ASCII-coded decimal value in (W) to an 8-bit
; binary value.
;
; On entry:
; W = pointer to the buffer to convert
;
; On return:
; A = converted value
; B, C clobbered
;
_acdbin8:
                stz C                   ; zero the result
                ldy #0
@loop:
                lda (W),y               ; get digit to convert
                beq @done               ; no more digits
                sec
                sbc #'0'                ; convert ASCII digit to binary
                clc
                adc C                   ; fold in ones place value
                iny
                sta C                   ; save result
@next:
                lda (W),y               ; get digit to convert
                beq @done               ; no more digits

                ; shift result left by one digit position
                lda C                   ; fetch result
                asl                     ; A' = 2*A
                sta B                   ; B = A'
                asl                     ; A' = 4*A
                asl                     ; A' = 8*A
                clc
                adc B                   ; A' = 8*A + 2*A = (8 + 2)*A = 10*A
                sta C                   ; store result
                bra @loop
@done:
                lda C
                rts


;-----------------------------------------------------------------------
; _pbcd16:
; Prints a 16-bit BCD value.
;
; On entry:
; AX = 16 bit value to print
;
; On return:
; A clobbered
;
_pbcd16:
                cmp #0                  ; is MSB zero?
                bne @print_msb          ; go print it
                lda #SPC                ; print spaces
                sta CONSOLE_IO          ; instead of
                sta CONSOLE_IO          ; leading zeros
                bra @print_lsb
@print_msb:
                jsr _pbcd8              ; print MSB
                txa
                jsr _pbcd8u             ; print LSB
                rts
@print_lsb:
                txa
                jsr _pbcd8              ; print LSB
                rts


;-----------------------------------------------------------------------
; _pbcd8:
; Prints an 8-bit BCD value.
;
; On entry:
; A = 8-bit value to print
;
; On return:
; A clobbered
;
                ; ====== This entry point prints a leading space
                ; when leading digit is zero.
_pbcd8:
                pha
                and #$f0                ; is upper nibble non-zero?
                bne _pbcd_upper         ; go print it
                lda #SPC                ; print space instead
                sta CONSOLE_IO          ; of leading zero
                bra _pbcd_lower

                ; ====== This entry point skips the leading space
                ; when leading digit is zero.
_pbcd8n:
                pha
                and #$f0                ; is upper nibble non-zero?
                bne _pbcd_upper         ; go print it
                bra _pbcd_lower

                ; ====== This entry point prints a leading zero
                ; when leading digit is zero.
_pbcd8u:
                pha
_pbcd_upper:
                ; move upper nibble to lower nibble
                lsr
                lsr
                lsr
                lsr
                ; convert to ASCII digit
                clc
                adc #'0'
                sta CONSOLE_IO
_pbcd_lower:
                pla                     ; recover arg to print
                and #$0f                ; discard upper nibble
                ; convert to ASCII digit
                clc
                adc #'0'
                sta CONSOLE_IO
                rts


;-----------------------------------------------------------------------
; _phex16:
; Prints a 16-bit value as four hexadecimal digits
;
; On entry:
; AX contains the value to be printed
;
; On return:
; A clobbered
;
_phex16:
                jsr _phex8              ; print the MSB
                txa
                jsr _phex8              ; print the LSB
                rts


;-----------------------------------------------------------------------
; _phex8:
; Displays an 8-bit value as two hexadecimal digits
;
; On entry:
; A contains the value to be displayed
;
; On return:
; A clobbered
;
_phex8:
                pha                     ; preserve input value
                ; move upper nibble to lower nibble
                lsr
                lsr
                lsr
                lsr
                jsr _phex4              ; display upper nibble in hex
                pla                     ; recover input value
                jsr _phex4              ; display lower nibble in hex
                rts


;-----------------------------------------------------------------------
; _phex4:
; Displays a 4-bit value as a hexadecimal digit.
;
; On entry:
; Lower 4-bits of A contain the value to be displayed
;
_phex4:
                and #$f                 ; isolate lower nibble
                clc
                adc #'0'                ; A now in ['0'..)
                cmp #'9' + 1
                bcc @no_adjust          ; go if A in ['0'..'9']
                clc
                adc #7                  ; A now in ['A'..'F']
@no_adjust:
                sta CONSOLE_IO          ; display hex digit
                rts


                .segment "RODATA"

        .macro SGR_RESET
                .byte ESC,"[0m"
        .endmacro

        .macro BG_BLUE
                .byte ESC,"[44m"
        .endmacro

        .macro FG_RED
                .byte ESC,"[31m"
        .endmacro

        .macro FG_WHITE
                .byte ESC,"[37m"
        .endmacro

_clear_display:
                .byte ESC,"[0m",ESC,"[?7l",ESC,"[?25l",ESC,"[H",ESC,"[J",0

_reset_display:
                .byte ESC,"[0m",ESC,"[?25h",ESC,"[H",ESC,"[J",0

_absurd_cup:
                .byte CR, LF, ESC,"[999;999H",0

_cursor_position_request:
                .byte ESC, "[6n",0

_setup_error:
                .byte ESC,"[H",ESC,"[JCannot determine screen dimensions",CR,LF,0


_status_line_post:
_lives_post:
_timer_post:
_score_post:
_play_again_post:
_game_over_post:
                SGR_RESET
                .byte 0

_status_line_pre:
                BG_BLUE
                FG_WHITE
                .byte 0

_title_label:
                .byte "SNAKE!",0
                TITLE_WIDTH = 2 * (* - _title_label - 1)


_game_over_pre:
                BG_BLUE
                FG_WHITE
                .byte 0

_game_over_label:
                .byte "GAME OVER",0
                GAME_OVER_WIDTH = 2 * (* - _game_over_label - 1)

_lives_label:
                .byte "Lives",0
                LIVES_LABEL_OFFSET = -20
                LIVES_OFFSET = -14
_lives_pre:
                BG_BLUE
                FG_WHITE
                .byte 0
_score_label:
                .byte "Score",0
                SCORE_LABEL_OFFSET = -11
                SCORE_OFFSET = -5

_timer_label:
                .byte "Time",0
                TIMER_LABEL_OFFSET = 3
                TIMER_OFFSET = 8

_timer_pre:
                BG_BLUE
                FG_WHITE
                .byte 0

_score_pre:
                BG_BLUE
                FG_WHITE
                .byte 0

_play_again_pre:
                BG_BLUE
                FG_WHITE
                .byte 0

_play_again_label:
                .byte "(P)lay again",0
                PLAY_AGAIN_OFFSET = 3

_reset_color:
                SGR_RESET
                .byte 0
_red_foreground:
                FG_RED
                .byte 0
