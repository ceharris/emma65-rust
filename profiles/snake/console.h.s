        .ifndef CONSOLE_H
                CONSOLE_H = 1

            CONSOLE_IO = $FFF8
            CONSOLE_LATCH = $FFF9

            .global console_getcb
            .global console_getcp
            .global console_getcw
            
            .global console_puts
            .global console_putsw
            .global console_putsc

            .global console_drain


        .endif
