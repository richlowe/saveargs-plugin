# Broken save-args as a GCC plugin

This doesn't work, it's here to show people things.  It's derived from a lot
of random guesswork and googling and whatnot.  It is by necessity GPL.

1) It's _very_ fragile depending on optimization level, etc.
   The data always(?) lands, but the instructions that get it there won't
   please libsaveargs except at `-O2`
2) I cannot find a way to get our data _before_ locals on the stack, where
   they must be.

It's possible these are inherent limitations of doing this in GIMPLE, but if
we do it in RTL is there really an advantage to a plugin v. patching the
compiler?

Also can we even do it in RTL?  It seems like by the time RTL has a
prologue/epilogue the relationship between variables and locations on the
stack is set, and we can't write to where we need to, only _after_ local
variables (the same as with GIMPLE).  (This is true, we need to either be
patched into the pro/epi code, or we can't work that way).

Potentially(?) we could add a DWARF tag to say where the block of memory is,
which would partially, if complicatedly, solve #2 but not #1.
