CPPFLAGS += -I/opt/gcc-10/lib/gcc/x86_64-pc-solaris2.11/10.5.0/plugin/include/
CPPFLAGS += -I/usr/include/gmp

save-args.so: save-args.cc
	/opt/gcc-10/bin/g++ -shared -fPIC -fno-rtti $(CPPFLAGS) $^ -o $@

test: test.c
	/opt/gcc-10/bin/gcc -fplugin=./save-args.so -g -O2 $^ -o $@
	ctfconvert $@

.KEEP_STATE:

clean:
	rm save-args.so test
