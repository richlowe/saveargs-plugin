# Broken save-args as a GCC plugin

This doesn't work, it's here to show people things.  It's derived from a lot
of random guesswork and googling and whatnot.  It is by necessity GPL.

1) It's _very_ fragile depending on optimization level, etc.
   The data always(?) lands, but the instructions that get it there won't
   please libsaveargs except at `-O2`.
2) I cannot find a way to get our data _before_ locals on the stack, where
   they must be.

In combination, these mean that the traditional `-msave-args`/`-Wu,save_args`
protocol (henceforth version 0) can't be used and we have to come up with an
alternative.

What we do is this (version 1):

- Create an array `void *__illumos_saved_args_v1__[]` in each function
- for each parameter store its value in the array when locals are initialized

This reduces our problem space from finding _N_ parameters each of which may
have been clobbered to finding one local variable that we can guarantee has
not been clobbered.

We end up with DWARF which looks like this:

```
0x00000061:   DW_TAG_subprogram
                DW_AT_external	(0x01)
                DW_AT_name	("main")
                DW_AT_decl_file	("/builds/richlowe/saveargs-plugin/test.c")
                DW_AT_decl_line	(4)
                DW_AT_decl_column	(0x01)
                DW_AT_prototyped	(0x01)
                DW_AT_type	(0x0000005a "int")
                DW_AT_low_pc	(0x0000000000400fd0)
                DW_AT_high_pc	(0x0000000000401009)
                DW_AT_frame_base	(0x00000000:
                   [0x0000000000400fd0, 0x0000000000400fd1): DW_OP_breg7 RSP+8
                   [0x0000000000400fd1, 0x0000000000400fd8): DW_OP_breg7 RSP+16
                   [0x0000000000400fd8, 0x0000000000401008): DW_OP_breg6 RBP+16
                   [0x0000000000401008, 0x0000000000401009): DW_OP_breg7 RSP+8)
                DW_AT_GNU_all_call_sites	(0x01)
                DW_AT_sibling	(0x00000105)
```

```
0x000000d4:     DW_TAG_variable
                  DW_AT_name	("__illumos_saved_args_v1__")
                  DW_AT_type	(0x00000131 "void *const volatile[3]")
                  DW_AT_artificial	(0x01)
                  DW_AT_location	(DW_OP_fbreg -1072)
```

Thus we know where our block starts relative to the frame base, and that the
block is never clobbered.

We also know where that frame base is -- although it wanders, it always has --
what the DWARF is telling us is:

 - use `%rsp+8` until we have pushed `%rbp` in the prologue
 - use `%rsp+16` (because of the `%rbp` push) until we have adjusted `%rsp` in
   the prologue
 - use `%rbp+16` after this (the function body), until the epilogue
    (the +16 is the return address and frame pointer).
 - use `%rsp+8` again in the epilogue after we've popped

This is a long-winded way of saying "If have a frame pointer, the frame base
is there" (which you would expect) and thus our saved block is at a constant
address relative to the frame pointer (after the frame is established).  This
is the exact situation we arranged to be true in version 0, except that the
offset between the frame pointer and the argument block is not static at
compile time, but must be determined from debug information.


A simple `main` function looks like this:

```
(gdb) disassemble main
Dump of assembler code for function main:
   0x0000000000400fd0 <+0>:	push   %rbp
   0x0000000000400fd1 <+1>:	mov    %edi,%edi
   0x0000000000400fd3 <+3>:	xor    %eax,%eax
   0x0000000000400fd5 <+5>:	mov    %rsp,%rbp
   0x0000000000400fd8 <+8>:	sub    $0x420,%rsp
   0x0000000000400fdf <+15>:	mov    %rdi,-0x410(%rbp)
   0x0000000000400fe6 <+22>:	mov    $0x40102d,%edi
   0x0000000000400feb <+27>:	mov    %rsi,-0x418(%rbp)
   0x0000000000400ff2 <+34>:	lea    -0x400(%rbp),%rsi
   0x0000000000400ff9 <+41>:	mov    %rdx,-0x420(%rbp)
   0x0000000000401000 <+48>:	call   0x400e38 <printf@plt>
   0x0000000000401005 <+53>:	xor    %eax,%eax
   0x0000000000401007 <+55>:	leave
   0x0000000000401008 <+56>:	ret
End of assembler dump.
(gdb)
```

So we can see we're done pushing arguments at +48 (not as soon as we'd like!)

```
(gdb) where
#0  main (argc=1, argv=0xfffffc7fffdfe828, envp=0xfffffc7fffdfe838)
    at test.c:5
```

and this is what the params look like, in a DWARF-ful world.


Break where we know we have finished pushing parameters:

```
(gdb) break *(main+48)
```

Now we know from the DWARF data that our "frame base" is at `%rbp + 16` and that
this is really `%rbp` (the +16 being the return address and saved frame
pointer).

And we know that our saved argument block is at `<frame base> - 1072`.

We should then find our argument block at `%rbp + 16 - 1072` which is
`%rbp - 1046` which is `%rbp - 0x420`, which we can see in the disassembly.

If we now print 3 pointer-sized-things at that location:

```
(gdb) x/3g ($rbp - 0x420)
0xfffffc7fffdfe3b0:	0xfffffc7fffdfe838	0xfffffc7fffdfe828
0xfffffc7fffdfe3c0:	0x0000000000000001
```

We find our arguments (reversed, currently, for historical reasons)

```
(gdb) x/xg 0xfffffc7fffdfe828
0xfffffc7fffdfe828:	0xfffffc7fffdfead8
(gdb) x/s 0xfffffc7fffdfead8
0xfffffc7fffdfead8:	"/builds/richlowe/saveargs-plugin/test"
(gdb) x/xg 0xfffffc7fffdfe838
0xfffffc7fffdfe838:	0xfffffc7fffdfeafe
(gdb) x/s 0xfffffc7fffdfeafe
0xfffffc7fffdfeafe:	"SHELL=/bin/zsh"
```

You could see how, given `ctfconvert(1)` (and CTF) changes whereby each
function type was annotated to say that this function has a saved argument
block at offset `N` from the frame pointer, we could recover this easily in
the places we currently utilize saved arguments.
