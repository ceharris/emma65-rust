	.ifndef MODEL_H
		MODEL_H = 1

		.include "state.h.s"

		.global model_init
		.global model_reset
		.global model_key_event
		.global model_next
		.global model_place_food
		.global model_withdraw_food
		
	.endif