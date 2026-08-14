
		.include "prng.h.s"
		.include "state.h.s"

		LFSR_LATCH = $FFF6

		.segment "CODE"


;-----------------------------------------------------------------------
; rnd_seed:
; Seeds the psuedo random number sequence.
;
; On entry:
;	AY = seed value for the LFSR
;
;
rnd_seed:
		sty LFSR_LATCH
		sta LFSR_LATCH+1
		rts


;-----------------------------------------------------------------------
; rnd_range:
; Gets a psuedo-random number in the range [0..A-1]
;
rnd_range:
		phy
		sta B
		jsr rnd_next
		tya
@check:
		cmp B
		bcs @reduce
		ply
		rts
@reduce:
		sec
		sbc B
		bra @check

		
;-----------------------------------------------------------------------
; rnd_next16:
; Gets the next 16-bit value from the LFSR.
;
; On return:
; 	AY = psuedo-random value from the LFSR
;
rnd_next:		
		ldy LFSR_LATCH
		lda LFSR_LATCH+1
		rts