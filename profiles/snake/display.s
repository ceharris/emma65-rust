
				.include "ascii.h.s"
				.include "console.h.s"
				.include "delay.h.s"
				.include "display.h.s"
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

				.segment "CODE"

;-----------------------------------------------------------------------
; ui_clear:
; Displays the initial state of the game UI.
;
ui_clear:
				; clear display
				ldiw _clear_display
				jsr console_puts
				
				; start the status line
				ldiw _status_line_pre
				jsr console_puts

				; display empty status line
				lda #' '
				ldx #80
				jsr console_putsc
				
				; display timer label
				ldiw _timer_label
				jsr console_puts
				
				; display title label
				ldiw _title_cup
				jsr console_puts
				ldiw _title_label
				jsr console_putsw

				; display lives label
				ldiw _lives_label
				jsr console_puts

				; display score label
				ldiw _score_label
				jsr console_puts

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
ui_redraw:
				jsr ui_clear
				ldx #0
				ldy #0
				jsr ui_put_grid_cup
				ldiw game_grid
				ldy #GRID_ROWS
@next_row:
				ldx #GRID_COLUMNS
@next_column:
				UI_PUT_EMPTY_COLOR
				lda (W)
				beq @empty
				and #$80
				beq @food
				UI_PUT_SNAKE_COLOR
				UI_PUT_SNAKE_SEGMENT
				bra @until_done
@food:
				lda (W)
				adc #'0'		
				sta CONSOLE_IO
				sta CONSOLE_IO
				bra @until_done
@empty:
				UI_PUT_EMPTY_SEGMENT
@until_done:
				inc W
				bne @no_carry
				inc W+1
@no_carry:
				dex
				bne @next_column
				dey
				bne @next_row
				stz CONSOLE_LATCH
				rts


;-----------------------------------------------------------------------
; ui_update:
; Updates the UI display to match the current state of the game model
;
ui_update:
				lda game_flags
				asl			; carry = food change flag
				bcc @check_grow_count	
				asl			; carry = food waiting flag
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
				ldiw _play_again_pre
				jsr console_puts
				ldiw _play_again_label
				jsr console_puts
				ldiw _play_again_post
				jsr console_puts

				; display game over label
				ldiw _game_over_pre
				jsr console_puts
				ldiw _game_over_label
				jsr console_putsw
				ldiw _game_over_post
				jsr console_puts

				; display zero life count
				jsr ui_put_life_count
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
				ldiw _game_over_pre
				jsr console_puts
				ldx #2*GAME_OVER_LENGTH
				lda #SPC
				jsr console_putsc
				ldiw _game_over_post
				jsr console_puts
				bra @blink
@show_game_over:
				ldiw _game_over_pre
				jsr console_puts
				ldiw _game_over_label
				jsr console_putsw
				ldiw _game_over_post
				jsr console_puts
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
; ui_put_bloody_segment:
;
; On entry:
;	X = grid X coordinate
;	Y = grid Y coordinate
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
;	X = grid X coordinate
;	Y = grid Y coordinate
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
;	X = grid X coordinate
;	Y = grid Y coordinate
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
;	X = grid X coordinate
;	Y = grid Y coordinate
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
;	A = food value
;	X = grid X coordinate
;	Y = grid Y coordinate
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
; ui_put_grid_cup:
; Puts an ANSI CUP (cursor position) sequence representing a grid 
; coordinate pair into the serial output buffer.
;
; On entry:
;	X = grid X coordinate
;	Y = grid Y coordinate
;
.global ui_put_grid_cup
ui_put_grid_cup:
				UI_PUT_ANSI_CSI
				
				; row number is first
				tya						; Y coordinate to A
				inc						; rows start at 1 not 0
				jsr _bin8bcd			; convert to BCD in W
				lda W+1
				beq @y_lower			; not three digits, skip MSB
				jsr @pbcd8				; put MSB digits
				lda W
				jsr @pbcd8u				; put LSB digits
				bra @put_sep
@y_lower:
				lda W
				jsr @pbcd8				; put LSB digits
@put_sep:
				; put separator
				lda #';'
				sta CONSOLE_IO

				; column number is second
				txa						; X coordinate to A
				asl						; two screen columns per grid column
				inc						; columns start at 1 not 0
				jsr _bin8bcd			; convert to BCD in W
				lda W+1
				beq @x_lower			; not three digits, skip MSB
				jsr @pbcd8				; put MSB digits
				lda W
				jsr @pbcd8u				; put LSB digits
				bra @put_cup
@x_lower:
				lda W	
				jsr @pbcd8				; put LSB digits					
@put_cup:
				UI_PUT_ANSI_CUP
				rts

@pbcd8:
				pha
				and #$f0				; is upper nibble non-zero?
				bne @pbcd_upper			; go print it
				bra @pbcd_lower
				; this entry point prints a leading zero
@pbcd8u:
				pha
@pbcd_upper:
				; move upper nibble to lower nibble
				lsr
				lsr
				lsr
				lsr
				; convert to ASCII digit
				clc
				adc #'0'
				sta CONSOLE_IO
@pbcd_lower:
				pla						; recover arg to print
				and #$0f				; discard upper nibble
				; convert to ASCII digit
				clc
				adc #'0'
				sta CONSOLE_IO
				rts


;-----------------------------------------------------------------------
; ui_put_life_count:
; Displays the current life count in the status line
;
ui_put_life_count:
				ldiw _lives_pre
				jsr console_puts		
				lda lives
				clc
				adc #'0'
				sta CONSOLE_IO
				ldiw _lives_post
				jsr console_puts
				rts


;-----------------------------------------------------------------------
; ui_put_timer:
; Displays the current timer in the status line.
;
ui_put_timer:
				ldiw _timer_pre
				jsr console_puts		
				lda food_expires
				jsr _pbcd8
				ldiw _timer_post
				jsr console_puts
				rts


;-----------------------------------------------------------------------
; ui_put_score:
; Displays the current score in the status line.
;
ui_put_score:
				ldiw _score_pre
				jsr console_puts		
				lda score+1
				ldx score
				jsr _pbcd16
				ldiw _score_post
				jsr console_puts
				rts


;----------------------------------------------------------------------
; ui_dump_grid
; Dump the contents of the grid in hexdecimal.
;
ui_dump_grid:
				; clear display
				ldiw _clear_display
				jsr console_puts
				ldiw game_grid
				ldx #GRID_ROWS		; X = row count
@next_row:
				ldy #0			; Y = column index
@next_column:
				lda (W),y
				bne @non_empty
				UI_PUT_EMPTY_COLOR
				lda #'-'
				sta CONSOLE_IO
				sta CONSOLE_IO
				bra @check_column
@non_empty:
				UI_PUT_DUMP_COLOR
				lda (W),y
				jsr _phex8		; print cell in hexadecimal
@check_column:
				iny
				cpy #GRID_COLUMNS
				bne @next_column
				clc
				lda W
				adc #GRID_COLUMNS
				sta W
				bcc @no_carry
				inc W+1
@no_carry:
				lda #CR
				sta CONSOLE_IO
				lda #LF
				sta CONSOLE_IO
				dex
				bne @next_row
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

show_food:
				ldiw _status_line_pre
				jsr console_puts
				lda #SPC
				sta CONSOLE_IO
				sta CONSOLE_IO
				lda food_x
				jsr _phex8
				lda #','
				sta CONSOLE_IO
				lda food_y0
				jsr _phex8
				lda #SPC
				sta CONSOLE_IO
				lda food_addr_0+1
				ldx food_addr_0
				jsr _phex16
				lda #SPC
				lda #SPC
				sta CONSOLE_IO
				lda food_x
				jsr _phex8
				lda #','
				sta CONSOLE_IO
				lda food_y1
				jsr _phex8
				lda #SPC
				sta CONSOLE_IO
				lda food_addr_1+1
				ldx food_addr_1
				jsr _phex16		
				ldiw _status_line_post
				jsr console_puts
				rts

show_positions:
				ldiw _status_line_pre
				jsr console_puts
				lda #SPC
				sta CONSOLE_IO
				lda snake_head_x
				jsr _phex8
				lda #','
				sta CONSOLE_IO
				lda snake_head_y
				jsr _phex8
				lda #SPC
				sta CONSOLE_IO
				lda snake_head_addr+1
				ldx snake_head_addr
				jsr _phex16
				lda #SPC
				sta CONSOLE_IO
				lda snake_tail_x
				jsr _phex8
				lda #','
				sta CONSOLE_IO
				lda snake_tail_y
				jsr _phex8
				lda #SPC
				sta CONSOLE_IO
				lda snake_tail_addr+1
				ldx snake_tail_addr
				jsr _phex16
				ldiw _status_line_post
				jsr console_puts
				rts


;-----------------------------------------------------------------------
; _bin8bcd:
; Convert an 8-bit binary value in A to a two- or three-digit BCD
; value.
;
; On entry:
;	A = 8 bit value to convert
;
; On return:
;	W = BCD digits that correspond to A
;	B clobbered
;
_bin8bcd:
				phx

				; save value to convert
				sta B				
			
				; zero out the result buffer
				stz W				
				stz W+1

				ldx #8					; bit counter
				sed
@loop:
				asl B				; high order bit of arg to carry
				lda W
				adc W				; double LSB + carry
				sta W
				lda W+1
				adc W+1			; double MSB + carry
				sta W+1				
				dex
				bne @loop
				
				cld
				plx
				rts


;-----------------------------------------------------------------------
; _pbcd16:
; Prints a 16-bit BCD value.
;
; On entry:
;	AX = 16 bit value to print
;
; On return:
;	A clobbered
;
_pbcd16:
				cmp #0			; is MSB zero?
				bne @print_msb		; go print it
				lda #SPC		; print spaces
				sta CONSOLE_IO		;   instead of
				sta CONSOLE_IO		;   leading zeros
				bra @print_lsb	
@print_msb:
				jsr _pbcd8		; print MSB
				txa
				jsr _pbcd8u		; print LSB
				rts
@print_lsb:
				txa
				jsr _pbcd8		; print LSB
				rts


;-----------------------------------------------------------------------
; _pbcd8:
; Prints an 8-bit BCD value.
;
; On entry:
;	A = 8-bit value to print
;
; On return:
;	A clobbered
;
_pbcd8:
				pha
				and #$f0				; is upper nibble non-zero?
				bne _pbcd_upper			; go print it
				lda #SPC				; print space instead
				sta CONSOLE_IO			;   of leading zero
				bra _pbcd_lower
				; this entry point prints a leading zero
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
				pla						; recover arg to print
				and #$0f				; discard upper nibble
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
;	AX contains the value to be printed
;
; On return:
;	A clobbered
;
_phex16:
				jsr _phex8		; print the MSB
				txa
				jsr _phex8		; print the LSB
				rts


;-----------------------------------------------------------------------
; _phex8:
; Displays an 8-bit value as two hexadecimal digits
;
; On entry:
;	A contains the value to be displayed
;
; On return:
;	A clobbered
;
_phex8:
				pha			; preserve input value
				; move upper nibble to lower nibble
				lsr
				lsr
				lsr
				lsr
				jsr _phex4		; display upper nibble in hex
				pla			; recover input value
				jsr _phex4		; display lower nibble in hex
				rts	


;-----------------------------------------------------------------------
; _phex4:
; Displays a 4-bit value as a hexadecimal digit.
;
; On entry:
; 	Lower 4-bits of A contain the value to be displayed
;
_phex4:
				and #$f			; isolate lower nibble
				clc	
				adc #'0'		; A now in ['0'..)
				cmp #'9' + 1
				bcc @no_adjust		; go if A in ['0'..'9']
				clc
				adc #7			; A now in ['A'..'F']
@no_adjust:
				sta CONSOLE_IO		; display hex digit
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
				.byte ESC,"[0m",ESC,"[?25l",ESC,"[H",ESC,"[J",0

_reset_display:
				.byte ESC,"[0m",ESC,"[?25h",ESC,"[H",ESC,"[J",0

_status_line_post:
_lives_post:
_timer_post:
_score_post:
_play_again_post:
_game_over_post:
				SGR_RESET
				.byte 0

_status_line_pre:
				.byte ESC,"[24;1H"
				BG_BLUE
				FG_WHITE
				.byte 0

_title_cup:
				.byte ESC,"[24;34H",0

_title_label:
				.byte "SNAKE!",0

_game_over_pre:
				.byte ESC,"[24;32H"
				BG_BLUE
				FG_WHITE
				.byte 0

_game_over_label:
				.byte "GAME OVER",0
				GAME_OVER_LENGTH = * - _game_over_label

_lives_label:
				.byte ESC,"[24;60HLives 0",0

_lives_pre:
				.byte ESC,"[24;66H"
				BG_BLUE
				FG_WHITE
				.byte 0

_score_label:
				.byte ESC,"[24;69HScore    0",0

_timer_label:
				.byte ESC,"[24;3HTime ",0

_timer_pre:
				.byte ESC,"[24;8H"
				BG_BLUE
				FG_WHITE
				.byte 0

_score_pre:
				.byte ESC,"[24;75H"
				BG_BLUE
				FG_WHITE
				.byte 0

_play_again_pre:
				.byte ESC,"[24;3H"
				BG_BLUE
				FG_WHITE
				.byte 0

_play_again_label:
				.byte "(P)lay again",0

_reset_color:
				SGR_RESET
				.byte 0
_red_foreground:
				FG_RED
				.byte 0
