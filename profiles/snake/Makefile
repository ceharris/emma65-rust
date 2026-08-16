.PHONY: all clean
.SUFFIXES: .bin .hex .lbl .lst .map .o .s .s19 .sym .tmp

EXECUTABLE=snake
PROFILE=$(HOME)/.emma/debugger/profiles/$(EXECUTABLE)
TEMPLATE=../../src/emulator/config/templates/$(EXECUTABLE)
LOAD_OFFSET=0xE000
EXECUTION_START_LABEL=start
LD_CONFIG=$(EXECUTABLE).cfg
BIN_IMAGE=$(EXECUTABLE).bin
ENTRY_TMP=entry.tmp

INCLUDES=$(LD_CONFIG) ascii.h.s delay.h.s display.h.s keys.h.s model.h.s prng.h.s \
	state.h.s timer.h.s
MODULES=snake.o console.o delay.o display.o keys.o model.o prng.o state.o \
	timer.o vectors.o

AS=ca65
ASFLAGS=--cpu W65C02 -I ../include
LD=ld65
LDFLAGS=--config $(LD_CONFIG)

all: template

profile: $(BIN_IMAGE)
	cp $(BIN_IMAGE) $(PROFILE)/program.bin
	cp $(EXECUTABLE).lbl $(PROFILE)/program.lbl

template: $(BIN_IMAGE)
	cp $(BIN_IMAGE) $(TEMPLATE)/program.bin
	cp $(EXECUTABLE).lbl $(TEMPLATE)/program.lbl

clean:
	-rm -f *.bin *.hex *.lbl *.lst *.map *.o *.s19 *.sym *.tmp

console.o: console.s $(INCLUDES)
delay.o: delay.s $(INCLUDES)
display.o: display.s $(INCLUDES)
keys.o: keys.s $(INCLUDES)
model.o: model.s $(INCLUDES)
prng.o: prng.s $(INCLUDES)
snake.o: snake.s $(INCLUDES)
timer.o: timer.s $(INCLUDES)
vectors.o: vectors.s $(INCLUDES)

$(BIN_IMAGE): $(MODULES)
	$(LD) $(LDFLAGS) $^ -o $@ -m $*.map -Ln $*.lbl

%.o: %.s
	$(AS) $(ASFLAGS) $< -o $@ -l $*.lst

%.o: ../lib/%.s
	$(AS) $(ASFLAGS) $< -o $@ -l $*.lst

	
