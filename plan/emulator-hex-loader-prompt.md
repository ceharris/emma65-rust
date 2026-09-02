It would be helpful to have the option of loading a memory device (RAM or ROM) 
from either an Intel Hex file or a Motorola S-Record file. I want your help in
creating a plan for adding this capability, but I plan to implement it myself 
to help develop my Rust knowledge and skills.

The Intel Hex loader should support record types 00 Data, 01 End of File, and 
05 Start Linear Address. When reading a file, if the first character
of the file is not the start-of-record indicator (:), it should consume text
until it reaches a newline (optional CR followed by required LF). This should
be repeated until the first start-of-record (:) is read. Subsequently, the
file must consist exclusively of legitimate hex records. The uppermost sixteen
bits of the start address, if present, are ignored.

The S-Record loader should support the so-called S19 subset, consisting of types
S0 Header, S1 Data, S5 Count, and S9 Start Address. Similar to the Intel Hex
loader, if the first character in the file is not 'S', it should scan the file
looking for the first occurrence of 'S' preceded by newline (optional CR followed
by required LF). Subsequently, the file must consist exclusively of legitimate
S-records.

Unlike binary image files, a hex loader can load arbitrary sequences of data bytes
at arbitrary offsets within the target memory (u8 vector) space. The hex file 
need not write every cell within the target memory. When loading a hex file, the
target u8 vector must be initialized such that its length corresponds to the 
intended memory size. The data in the vector could be either random (simulating
real RAM at power-on) or filled with a user-specified u8 value (e.g. 0xff to 
simulate the default state of EEPROM memory). 

The addresses specified by each data record in a hex file may represent the origin 
address specified for an assembler. For example, a hex file representing a program
that was assembled for location at 0xc000, could be loaded into a 32K memory segment
to be mapped at 0x8000 on the bus. The loader will need to offset the addresses in
the hex file by the start address of the target memory segment. Additionally, the
loader must ensure that all records lie within the size bounds of the target memory
segment. The loader should not attempt to detect hex records that overlap within
the target memory segment.

I want to determine the expected data type for a file using the filename suffix. 
Binary files must have a `.bin` suffix; Intel Hex must have a `.hex` or `.ihx` 
suffix. S-Record files must have a `.s19` suffix. The loader should have a
single entry point that uses the suffix of the provided filename (in a PathBuf) 
to determine which type of load to perform. Additional arguments to the load
entry point will include a mutable Vec<u8> (filled to the desired memory segment 
size), and the offset address (start address of the range at which the memory 
will be mapped). The offset address is used to bias the addresses in each record
of a hex file; it is unused when loading a binary file. The binary file load
should continue to validate that the image size matches the memory segment size,
as in the existing implementation.
