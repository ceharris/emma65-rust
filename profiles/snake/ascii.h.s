	.ifndef ASCII_H
		ASCII_H = 1

		NUL = $0
		SOH = $1
		ETX = $3
		EOT = $4
		ACK = $6
		BEL = $7
		BS = $8
		HT = $9
		LF = $A
		FF = $C
		CR = $D
		DC1 = $11
		DC2 = $12
		DC3 = $13
		NAK = $15
		CAN = $18
		ESC = $1B
		SPC = $20
		DEL = $7F

		NL = LF
		TAB = HT
		
		CTRL_C = ETX
		CTRL_D = EOT
		CTRL_Q = DC1
		CTRL_R = DC2
		CTRL_S = DC3
		CTRL_U = NAK
		CTRL_X = CAN
		
	.endif
