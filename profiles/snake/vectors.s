
		.global start
		.global timer_isr

		.segment "CODE"
noop_isr:
		rti
	
		.segment "MACHVECS"
		.word noop_isr
		.word start
		.word timer_isr