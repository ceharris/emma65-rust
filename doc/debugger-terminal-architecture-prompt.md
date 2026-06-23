I'd like to discuss the underlying architecture of the emulated terminal, 
in some detail. 

I'm assuming that `Xterm.js` will be used as the front-end component to 
realize the terminal UI.

I'm also assuming that `PipeTransport`, provided in the emulator's 
transport module, will be used as the means to convey terminal keyboard   
input and display output between the UI component and the virtual console 
device configured in the emulated bus. The emulator's transport layer is 
designed using a Tokio crossbeam channel to interface the synchronous 
concurrent emulated CPU execution with asynchronous, non-blocking transport
endpoints. 

I'd like to understand how you propose to interface the terminal UI 
component to the PipeTransport, with a design that ensures good 
performance characteristics.