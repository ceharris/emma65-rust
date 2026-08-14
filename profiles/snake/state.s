		.include "state.h.s"

		.segment "ZEROPAGE"

B:
		.res 1
W:
		.res 2
loop_delay:
		.res 2
loop_timer:
		.res 2
snake_head_x:
		.res 1
snake_head_y:
		.res 1
snake_tail_x:
		.res 1
snake_tail_y:
		.res 1
next_head_x:
		.res 1
next_head_y:
		.res 1
prev_head_x:
		.res 1
prev_head_y:
		.res 1
prev_tail_x:
		.res 1
prev_tail_y:
		.res 1
food_x:
		.res 1
food_y0:
		.res 1
food_y1:
		.res 1
game_flags:
		.res 1
grow_count:
		.res 1
lives:
		.res 1
snake_head_addr:
		.res 2
snake_tail_addr:
		.res 2
next_head_addr:
		.res 2
food_addr_0:
		.res 2
food_addr_1:
		.res 2
food_last:
		.res 1
food_expires:
		.res 1
score:
		.res 2


		.segment "BSS"
game_grid:
		.res GRID_CELLS
