                .include "state.h.s"

                .segment "ZEROPAGE"

B:
                .res 1
C:
                .res 1
W:
                .res 2
grid_width:
                .res 1
grid_height:
                .res 1
screen_width:
                .res 1
screen_height:
                .res 1
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
heap_end:
                .res 2
grid_size:
                .res 2
grid_y_table:
                .res 2
grid_base:
                .res 2


                .segment "HEAP"
heap_base:
                .res 1


                .segment "CODE"

;-----------------------------------------------------------------------
; heap_init:
; Initializes the heap bump allocator.
;
heap_init:
                lda #<heap_base
                sta heap_end
                lda #>heap_base
                sta heap_end+1
                rts


;-----------------------------------------------------------------------
; heap_alloc:
; Allocates memory via the heap's bump allocator.
;
; On entry:
; W = number of bytes to allocate
;
; On return:
; AY = address of the allocation
;
heap_alloc:
                lda heap_end
                pha
                clc
                adc W
                sta heap_end
                lda heap_end+1
                pha
                adc W+1
                sta heap_end+1
                pla
                ply
                rts

