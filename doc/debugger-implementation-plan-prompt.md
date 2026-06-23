
I want a plan for implementing the debugger that proceeds incrementally,
as a sequence of user stories. Each story should be focused on addressing 
a particular feature or capability needed by a user, and for which user
acceptance criteria can be stated in simple, easy-to-understand, and 
easy-to-evaluate terms. The changes to source code needed to implement any 
given user story should be small enough to admit thorough human review via 
a pull request.

After considering the stories and priorities given here, please follow up 
with your questions in an interactive dialogue. When offering several 
choices for the plan, please simply state the choices and allow me to 
respond in a free-form manner.

UI Feature Priorities
---------------------

Initial stories should focus on getting scaffolding in place such as 
creating the additional binary crate that uses a predefined config.toml to 
specify configuration for the emulator before launching the debugger UI. The
predefined configuration should be loaded from `~/.emma/debugger/default/emulator.toml`.

Initially, the UI can simply support Dark mode, and defer the Auto and Light
support until later in the sequence, when more of the UI is in place.

The user's highest priority in the initial stages of the project is 
getting the emulated terminal in place, and using the free run mode to run 
a program on the emulated CPU and interact with it via the terminal. 
When the program running on the emulated CPU executes the STP instruction,
the emulator will stop, and the debugger process can simply exit cleanly 
until additional stories are completed to provide more control to the user.

User's priorities after the terminal functionality is in place are roughly 
as follows:

 1. Single-step execution with disassembly view and registers view; no 
    register editing yet.
 2. Basic memory view with user-selectable address and scrolling; no follow 
    mode yet.
 3. Stack view.
 4. Simple breakpoints with basic toggle control only
 5. Additional execution control: Auto-Step
 6. Additional execution control: free running (Run, Cont, Stop)
 7. Register editing.
 8. Initial watchpoints support; see section below for details on scope.
 9. Auto, Light, and Dark mode selection control and corresponding updates 
    to UI component display colors.
10. Memory view follow modes.
11. Advanced UI control for simple breakpoints.
12. Additional watchpoint support: Add, Remove
13. Additional watchpoint support: Edit. Compiler feedback on error 
    displayed and resolved by user before the watchpoint can be saved.
14. Additional watchpoint support: Enable/Disable
15. Additional watchpoint support: View variables
16. Additional watchpoint support: Change variable value

Initial Watchpoints Support
---------------------------

The watchpoint support (in `src/watch`) includes the ability to parse a 
sequence of watch expressions from a string. This provides a simple 
mechanism to persist watchpoint expressions on the filesystem. The initial 
watchpoints support can build upon this. The user can create a sequence of 
watchpoints in a file outside of debugger, until such time that the UI 
provides built-in support for watchpoint management.

Let's assume that watchpoints are stored in `~/.emma/debugger/default/watchpoints.emw`.

debugger startup, the file is compiled. If any errors occur during 
compilation, the errors are displayed to standard error and the debugger 
exits with a non-zero exit code.

For the initial watchpoints story, watchpoints are display-only. Long 
watchpoint expressions are collapsed and an ellipsis control can be used 
to temporarily display the full expression.

Subsequent stories will provide controls for adding, removing, and editing.
When changes to watchpoints are committed, the file is updated, ensuring 
that watchpoints are persistent for the next debugging session.

Future features could use a different watchpoints file per project, etc.

Looking Forward
---------------

The path for configuration files presumes a future state in which multiple 
concurrent projects may use the debugger. The `default` segment in the 
path names of configuration files is a placeholder for a project name slug.
The directory associated with a project will store the emulator 
configuration (as a TOML file), watchpoint expressions, 
persistent configuration of UI features via another TOML file.
