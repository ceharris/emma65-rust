
		.include "ascii.h.s"
		.include "console.h.s"
		.include "delay.h.s"
		.include "display.h.s"
		.include "keys.h.s"
		.include "model.h.s"
		.include "state.h.s"
		.include "timer.h.s"

		.segment "CODE"
		.global start
start:
		ldx #$ff
		txs
		jsr timer_start
		cli

		jsr ui_begin
		bcc ready

		stp

ready:
		jsr heap_init
		jsr model_alloc

play_again:
		jsr model_init
next_life:
		jsr model_reset
		jsr ui_clear		; initialize UI display
		jsr console_drain	; discard any early key presses

loop:
		jsr ui_update		; display current model state
		jsr model_next		; compute next model state
		bcs life_over		; go if snake bit itself
@scan:
		jsr scan
		bcc loop
		lda loop_timer
		bne @scan
		lda loop_timer+1
		bne @scan
		bra loop

scan:
		jsr key_scan		; scan for user key presses
		cmp #KEY_DUMP
		beq dump
		cmp #KEY_PLAY
		beq pause
		cmp #KEY_QUIT		
		beq game_over		; exit on QUIT key
		cmp #KEY_REDRAW
		beq redraw		; redraw the UI display
		jsr model_key_event	; interpret key as event
		rts

redraw:
		jsr ui_redraw
		jmp loop		
pause:
		jsr key_scan
		cmp #KEY_QUIT
		beq bye
		cmp #KEY_DUMP
		beq dump
		cmp #KEY_PLAY
		bne pause
		jmp loop

dump:
		jsr ui_dump_grid
@wait:
		jsr key_scan
		cmp #KEY_QUIT
		beq bye
		cmp #KEY_PLAY
		beq play_again
		cmp #KEY_DUMP
		bne @wait
		jsr ui_redraw
		jmp loop

life_over:
		dec lives		; lives--
		beq game_over		; used all lives
		jsr ui_life_over	; display life over
		DELAY $0
		; # double the current delay
		lda loop_delay
		asl
		sta loop_delay
		lda loop_delay+1
		rol
		sta loop_delay+1		
		cmp #$0a
		bcc @done
		lda #$0a
		sta loop_delay+1
		stz loop_delay
@done:
		jmp next_life		; still in it

game_over:
		jsr ui_game_over
@loop:
		jsr ui_play_again	; prompt user to play again
		jsr key_scan		; scan for user key presses
		beq @loop		; no key pressed
		cmp #KEY_QUIT
		beq bye
		cmp #KEY_REDRAW
		beq game_over		; redraw game over screen
		cmp #KEY_PLAY
		bne @loop
		jmp play_again

bye:
		jsr timer_stop
		jsr ui_exit
		stp
		jmp start


