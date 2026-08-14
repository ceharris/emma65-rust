
	.ifndef DELAY_H
		DELAY_H = 1

		.globalzp delay_lower16
		.globalzp delay_upper16
		.global delay16
		.global delay32

		.macro DELAY imm16
		lda #<imm16
		sta delay_lower16
		lda #>imm16
		sta delay_lower16+1
		jsr delay16
		.endmacro

 	.macro LONG_DELAY imm16h, imm16l
		lda #<(imm16l)
		sta delay_lower16
		lda #>(imm16l)
		sta delay_lower16+1
		lda #<(imm16h)
		sta delay_upper16
		lda #>(imm16h)
		sta delay_upper16+1
		jsr delay32
	.endmacro

	.endif
