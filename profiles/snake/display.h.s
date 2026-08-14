		.ifndef UI_H
				UI_H = 1

				UI_NONE = 0
				UI_UP = 1
				UI_RIGHT = 2
				UI_DOWN = 3
				UI_LEFT = 4
				UI_QUIT = 5
				UI_REDRAW = 6

				.global ui_clear
				.global ui_redraw
				.global ui_update
				.global ui_life_over
				.global ui_game_over
				.global ui_play_again	

				.global ui_dump_grid

				.global ui_exit
			
		.endif
