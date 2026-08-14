		.include "delay.h.s"

		.segment "ZEROPAGE"
delay_lower16:
		.res 2
delay_upper16:
		.res 2

		.segment "CODE"
delay16:
		dec delay_lower16
		bne delay16
		dec delay_lower16+1
		bne delay16
		rts
	
delay32:
		jsr delay16
		dec delay_upper16
		bne delay32
		lda delay_upper16+1
		beq @done
		dec delay_upper16+1
		bra delay32
@done:
		rts

