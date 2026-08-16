
                .include "model.h.s"
                .include "keys.h.s"
                .include "prng.h.s"
                .include "timer.h.s"

                .segment "CODE"


;-----------------------------------------------------------------------
; model_alloc:
; Allocates the grid and column vector of row addresses.
;
; On entry:
; grid_width = width of the grid in cells
; grid_height = height of the grid in cells
;
model_alloc:
                ; determine size of Y-offset table
                lda grid_height         ; number of grid rows
                asl                     ; numbef of Y-offset addresses
                ; set W = number of bytes to allocate
                sta W
                stz W+1
                jsr heap_alloc
                ; save pointer to Y-offset table
                sty grid_y_table
                sta grid_y_table+1

                ldx grid_height
                ldy #0
                stz grid_size
                stz grid_size+1
@loop1:
                lda grid_size
                sta (grid_y_table),y
                iny
                clc
                adc grid_width
                sta grid_size
                lda grid_size+1
                sta (grid_y_table),y
                iny
                adc #0
                sta grid_size+1
                dex
                bne @loop1

                ; grid_size is now grid_height * grid_width
                ; allocate space for the grid
                lda grid_size
                sta W
                lda grid_size+1
                sta W+1
                jsr heap_alloc
                sty grid_base
                sta grid_base+1

                ldx grid_height
                ldy #0
@loop2:
                lda (grid_y_table),y
                clc
                adc grid_base
                sta (grid_y_table),y
                iny
                lda (grid_y_table),y
                adc grid_base+1
                sta (grid_y_table),y
                iny
                dex
                bne @loop2
                rts


;-----------------------------------------------------------------------
; model_init:
; Configures the initial state for the game.
;
model_init:
                lda #0
                sta loop_delay
                sta loop_delay+1
                sta game_flags
                sta grow_count
                sta score
                sta score+1
                sta food_last
                sta food_expires
                lda grid_width
                lsr
                tax
                stx snake_head_x
                stx prev_head_x
                stx prev_tail_x
                lda grid_height
                lsr
                tay
                sty snake_head_y
                sty prev_head_y
                sty prev_tail_y
                jsr _grid_offset
                stx snake_head_addr
                sta snake_head_addr+1
                lda #NUM_LIVES
                sta lives
                rts

model_reset:
                jsr _empty_grid
                lda #0
                sta grow_count
                lda game_flags
                and #GF_DIRECTION_BITS
                sta game_flags
                ldx snake_head_x
                stx prev_head_x
                stx snake_tail_x
                ldy snake_head_y
                sty prev_head_y
                sty snake_tail_y
                jsr _grid_offset
                stx snake_tail_addr
                sta snake_tail_addr+1
                lda #$A
                sta loop_delay+1
                stz food_expires
                lda chrono_second
                sta food_last
                rts

;-----------------------------------------------------------------------
; model_next:
; Resolve the next state of the game model.
;
; On return:
; carry set if snake bit itself and died
;
model_next:
                lda loop_delay
                sta loop_timer
                lda loop_delay+1
                sta loop_timer+1
                jsr _age_food           ; age food if present
                jsr _update_head        ; determine new head location
                lda (next_head_addr)    ; fetch cell at new head location
                tax
                and #SNAKE_CELL         ; test for snake cell
                beq @not_snake          ; go if not a snake cell
                jsr _commit_head        ; commit new head location
                sec                     ; carry indicates snake killed itself
                rts
@not_snake:
                txa                     ; recover cell content
                beq @empty_cell         ; go if empty cell
                ; add food value to grow count
                clc
                lda grow_count
                adc (next_head_addr)
                sta grow_count          ; save new grow count
                stz food_expires
                lda game_flags
                and #<(~GF_FOOD_WAITING)
                ora #(GF_FOOD_CHANGE | GF_FOOD_CONSUMED | GF_TIMER_CHANGE)
                sta game_flags

                ; reduce the loop delay by food value
                sec
                lda loop_delay
                sbc (food_addr_0)
                sta loop_delay
                bcc @remove_food
                stz loop_delay
                lda loop_delay+1
                beq @remove_food
                dec loop_delay+1

                ; remove the food from the grid
@remove_food:
                lda #EMPTY_CELL
                sta (food_addr_0)
                sta (food_addr_1)
@empty_cell:
                ; put breadcrumb into current head cell
                lda game_flags          ; fetch flags
                and #GF_DIRECTION_BITS  ; mask off all but direction bits
                ora #SNAKE_CELL         ; set snake cell bit
                sta (snake_head_addr)   ; current cell now "points" to next

                ; put snake head into new head cell
                lda #SNAKE_HEAD_CELL
                sta (next_head_addr)

                ; is the snake now growing?
                lda grow_count          ; fetch grow count
                beq @not_growing        ; go if not growing
                dea                     ; reduce grow count
                sta grow_count          ; store new grow count
                lda #1
                jsr _incr_score
                bra @finish             ; finish without updating tail

@not_growing:
                jsr _update_tail        ; determine new tail position
@finish:
                jsr _commit_head        ; commit new head position

                ; place food if possible
                lda game_flags
                and #(GF_FOOD_CONSUMED | GF_FOOD_WAITING)
                bne @done
                lda grow_count
                bne @done               ; don't place food if growing
                jsr _place_food
@done:
                lda game_flags
                and #<(~GF_FOOD_CONSUMED)
                sta game_flags
                clc                     ; snake still alive
                rts


;-----------------------------------------------------------------------
; model_key_event:
; Updates the model according to a received key event.
;
; On entry:
; A = key code
;
model_key_event:
                cmp #KEY_UP
                beq @move_up
                cmp #KEY_RIGHT
                beq @move_right
                cmp #KEY_DOWN
                beq @move_down
                cmp #KEY_LEFT
                beq @move_left
@wait:
                dec loop_timer
                bne @done
                lda loop_timer+1
                beq @done
                dec loop_timer+1
@done:
                sec
                rts

@move_up:
                lda game_flags
                ora #<(GF_AXIS_VERTICAL | GF_OP_DECREMENT)
                sta game_flags
                clc
                rts
@move_right:
                lda game_flags
                and #<~(GF_AXIS_VERTICAL | GF_OP_DECREMENT)
                sta game_flags
                clc
                rts
@move_down:
                lda game_flags
                and #<~GF_OP_DECREMENT
                ora #<GF_AXIS_VERTICAL
                sta game_flags
                clc
                rts
@move_left:
                lda game_flags
                and #<~GF_AXIS_VERTICAL
                ora #<GF_OP_DECREMENT
                sta game_flags
                clc
                rts


;-----------------------------------------------------------------------
; model_get_cell:
; Gets the content of the cell at (X,Y)
;
; On entry:
; X = X-coordinate in the grid
; Y = Y-coordinate in the gird
;
; On return:
; A = contents of grid cell at (X,Y)
; W clobbered
;
model_get_cell:
                phx
                phy

                ; set W = offset of the cell in the grid
                jsr _grid_offset

                ; fetch the contents of the cell
                lda (W)

                ply
                plx
                ora #0                  ; set Z if A=0
                rts


;-----------------------------------------------------------------------
; _empty_grid:
; Initializes the game grid by marking all cells as empty.
;
                .global _empty_grid
_empty_grid:
                lda grid_base
                sta W
                lda grid_base+1
                sta W+1
                ldx grid_size+1         ; number of full pages
                bne @multi_page

                ; just one page -- pick up at the remainder case
                ldx grid_size           ; size of partial page
                bra @remainder_loop

                ; multiple-pages
@multi_page:
                ldy #0
                lda #EMPTY_CELL
@page_loop:
                sta (W),y
                iny
                bne @page_loop
                inc W+1                 ; next page
                dex
                bne @page_loop

                ldx grid_size           ; size of partial page
@remainder_loop:
                sta (W),y
                iny
                dex
                bne @remainder_loop
                rts


;-----------------------------------------------------------------------
; _update_head:
; Update the head of the snake.
; Coordinates of the new head cell are determined using the current
; head coordinates and the bit-mapped game flags.
;
; On entry:
; snake_head_x, snake_head_y are the current head coordinates
;
; On return:
; next_head_x, next_head_y are the new head coordinates
; next_head_addr is the grid offset of the new head cell
; snake_head_x, snake_head_y is unchanged
; snake_head_addr is unchanged
; grid content is unchanged
;
_update_head:
                ldx snake_head_x
                ldy snake_head_y
                ; use game_flags to determine next head cell
                lda game_flags          ; get flags
                ; set carry to vertical/horizontal flag
                lsr
                bcs @vertical_next      ; go if next is on vertical axis
                ; set carry to decrement/increment flag
                lsr
                bcs @dec_horizontal     ; go if decrementing
                jsr _incr_horizontal    ; increment X coordinate with wrap
                bra @next_head_addr
@dec_horizontal:
                jsr _decr_horizontal    ; decrement X coordinate with wrap
                bra @next_head_addr
@vertical_next:
                ; set carry to decrement/increment flag
                lsr
                bcs @dec_vertical       ; go if decrementing
                jsr _incr_vertical      ; increment Y coordinate with wrap
                bra @next_head_addr
@dec_vertical:
                jsr _decr_vertical      ; decrement Y coordinate with wrap

@next_head_addr:
                stx next_head_x
                sty next_head_y
                jsr _grid_offset
                stx next_head_addr
                sta next_head_addr+1
                rts


;-----------------------------------------------------------------------
; _commit_head:
; Commits the computed next head position as the new head position.
;
_commit_head:
                lda snake_head_x
                sta prev_head_x
                lda next_head_x
                sta snake_head_x
                lda snake_head_y
                sta prev_head_y
                lda next_head_y
                sta snake_head_y
                lda next_head_addr
                sta snake_head_addr
                lda next_head_addr+1
                sta snake_head_addr+1
                rts

;-----------------------------------------------------------------------
; _update_tail:
; Update the tail of the snake.
; Coordinates of the new tail cell are determined using the current
; tail cell coordinates and the bit-mapped flags in the current cell
; itself.
;
; On entry:
; (snake_tail_x, snake_tail_y) are the current tail coordinates
;
; On return:
; (prev_tail_x, prev_tail_y) are the tail coordinates on entry
; (snake_tail_x, snake_tail_y) are the new tail coordinates
; (snake_tail_addr) is the grid offset of the new tail cell
; previous cell is empty
;
_update_tail:
                ; save snake tail coordinates
                lda snake_tail_x
                sta prev_tail_x
                tax                     ; X = current tail X
                lda snake_tail_y
                sta prev_tail_y
                tay                     ; Y = current tail Y

                ; use tail cell content to determine next tail cell
                lda (snake_tail_addr)   ; get contents of tail cell
                lsr
                bcs @vertical_next      ; go if next is on vertical axis
                lsr                     ; go if decrementing
                bcs @dec_horizontal
                jsr _incr_horizontal    ; increment X coordinate with wrap
                bra @next_tail_addr
@dec_horizontal:
                jsr _decr_horizontal    ; decrement X coordinate with wrap
                bra @next_tail_addr
@vertical_next:
                lsr                     ; set carry if decrementing
                bcs @dec_vertical
                jsr _incr_vertical      ; increment Y coordinate with wrap
                bra @next_tail_addr
@dec_vertical:
                jsr _decr_vertical      ; decrement Y coordinate with wrap

@next_tail_addr:
                stx snake_tail_x
                sty snake_tail_y

                ; empty the current tail cell
                lda #EMPTY_CELL
                sta (snake_tail_addr)
                jsr _grid_offset
                stx snake_tail_addr
                sta snake_tail_addr+1
                rts


;-----------------------------------------------------------------------
; _age_food:
; Ages food that is waiting to be eaten.
;
_age_food:
                lda chrono_second
                cmp food_last
                bne @check_for_food
                rts

@check_for_food:
                ; is food waiting?
                lda game_flags
                and #GF_FOOD_WAITING
                bne @food_waiting
                rts
@food_waiting:
                lda food_expires
                sed
                sec
                sbc #1
                cld
                sta food_expires
                bne @not_expired

                ; withdraw food
                lda #EMPTY_CELL
                sta (food_addr_0)
                sta (food_addr_1)
                lda game_flags
                and #<~(GF_FOOD_WAITING)
                ora #(GF_FOOD_CHANGE|GF_FOOD_CONSUMED)
                sta game_flags
                sec
                bra @done
@not_expired:
                lda chrono_second
                sta food_last
                lda game_flags
                ora #GF_TIMER_CHANGE
                sta game_flags
@done:
                rts


;-----------------------------------------------------------------------
; _place_food:
; Places food into the grid.
;
_place_food:
                lda #3+1
                sta B
@try_again:
                dec B
                bne @choose
                rts
@choose:
                ; randomly choose a column
                lda grid_width
                jsr rnd_range
                sta food_x
                tax
                ; randomly choose a row
                lda grid_height
                jsr rnd_range
                sta food_y0
                tay
                ; compute and save grid offset
                jsr _grid_offset
                stx food_addr_0
                sta food_addr_0+1
                ; want an empty cell
                lda (food_addr_0)
                bne @try_again
                ; is the cooresponding cell in the next row empty?
                ldy food_y0
                jsr _incr_vertical
                sty food_y1
                ldx food_x
                jsr _grid_offset
                stx food_addr_1
                sta food_addr_1+1
                lda (food_addr_1)
                beq @found
                ldy food_y0
                jsr _decr_vertical
                sty food_y1
                ldx food_x
                jsr _grid_offset
                stx food_addr_1
                sta food_addr_1+1
                lda (food_addr_1)
                bne @try_again
@found:
                ; place food into selected grid cells
                lda #9
                jsr rnd_range           ; choose 0 <= food value <= 8
                ina                     ; now 1 <= food value <= 9
                sta (food_addr_0)       ; put food value into first row
                sta (food_addr_1)       ; put food value into other row
                ; choose expiration between 3 and 10 seconds
                lda #8
                jsr rnd_range           ; choose <= TTL < 8
                sed
                clc
                adc #3                  ; now 3 <= TTL <= 10
                cld
                sta food_expires
                ; save current time
                lda chrono_second
                sta food_last
                ; set flags to indicate food is available
                lda game_flags
                ora #(GF_FOOD_WAITING|GF_FOOD_CHANGE|GF_TIMER_CHANGE)
                sta game_flags

                rts


;-----------------------------------------------------------------------
; _grid_offset:
; Compute the grid offset address for an XY coordinate pair.
;
; On entry:
; X, Y = grid coordinate pair
;
; On return:
; AX = address offset
; W = address offset
;
_grid_offset:
                ; set Y = 2*Y, the offset within the column vector
                tya
                asl
                tay

                ; set W = starting address of the row
                lda (grid_y_table),y
                sta W
                iny
                lda (grid_y_table),y
                sta W+1

                ; add the column to get the address of the cell and store in W
                txa                     ; A = X coordinate
                clc
                adc W                   ; W = column address (LSB only)
                sta W
                tax                     ; X = column address LSB
                bcc @no_carry
                inc W+1                 ; update MSB if needed
@no_carry:
                lda W+1                 ; A = column address MSB
                rts


;-----------------------------------------------------------------------
; _incr_horizontal:
; Increments a horizontal grid coordinate in the X register, modulo
; the number of grid columns.
;
; On entry:
; X = grid column
; On return:
; X = (X' + 1) mod grid_width
;
_incr_horizontal:
                inx
                cpx grid_width
                bcc @done
                ldx #0
@done:
                rts


;-----------------------------------------------------------------------
; _decr_horizontal:
; Decrements a horizontal grid coordinate in the X register, modulo
; the number of grid columns.
;
; On entry:
; X = grid column
; On return:
; X = (X' - 1) mod grid_width
;
_decr_horizontal:
                dex
                bmi @wrap
                rts
@wrap:
                ldx grid_width
                dex
                rts


;-----------------------------------------------------------------------
; _incr_vertical:
; Increments a vertical grid coordinate in the Y register, modulo
; the number of grid rows.
;
; On entry:
; Y = grid row
; On return:
; Y = (Y' + 1) mod grid_height
;
_incr_vertical:
                iny
                cpy grid_height
                bcc @done
                ldy #0
@done:
                rts


;-----------------------------------------------------------------------
; _decr_vertical:
; Decrements a vertical grid coordinate in the Y register, modulo
; the number of grid rows.
;
; On entry:
; Y = grid row
; On return:
; Y = (Y' - 1) mod grid_height
;
_decr_vertical:
                dey
                bmi @wrap
                rts
@wrap:
                ldy grid_height
                dey
                rts


;-----------------------------------------------------------------------
; _incr_score:
; Increment the player's score and set the score's dirty flag so that the
; UI representation is updated at the next refresh.
;
; On entry:
; A = amount by which the score should be increased
;
; On return:
; (score) = (score)' + A'
; (game_flags) = (game_flags)' | GF_SCORE_CHANGE
; A = (game_flags)' | GF_SCORE_CHANGE
;
_incr_score:
                ; add A to (score) using BCD
                sed
                clc
                adc score               ; increment LSB
                sta score
                lda #0
                adc score+1             ; propgate carry to MSB
                sta score+1
                cld
                bra _set_score_flag


;-----------------------------------------------------------------------
; _decr_score:
; Decrement the player's score and set the score's dirty flag so that the
; UI representation is updated at the next refresh. If the resulting
; score would be negative, it is instead set to zero.
;
; On entry:
; A = amount by which the score should be decreased
;
; On return:
; (score) = (score)' - A'
; (game_flags) = (game_flags)' | GF_SCORE_CHANGE
; A = (game_flags)' | GF_SCORE_CHANGE
; B clobbered
;
_decr_score:
                ; subtract A from (score) using BCD
                sta B
                sed
                sec
                lda score
                sbc B
                sta score
                stz B
                lda score+1
                sbc B
                sta score+1
                cld
                bcs _set_score_flag
                ; result would be negative; reset score to zero
                stz score
                stz score+1
                bra _set_score_flag


;-----------------------------------------------------------------------
; _set_score_flag:
; Sets the change flag for the score so that the UI representation will
; be updated at the next refresh.
;
; On return:
; A = (game_flags) | GF_SCORE_CHANGE
;
_set_score_flag:
                lda game_flags
                ora #GF_SCORE_CHANGE
                sta game_flags
                rts