CC_CPPFLAGS = $(TARGET_CXX) --print-file plugin

CPPFLAGS += -I$(CC_CPPFLAGS:sh)/include/
CPPFLAGS += -I/usr/include/gmp

save-args.so: ../common/save-args.cc
	$(NATIVE_CXX) -shared -fPIC -fno-rtti $(CPPFLAGS) $^ -o $@

test: ../common/test.c save-args.so
	$(TARGET_CC) -fplugin=./save-args.so -g -O2 ../common/test.c -o $@
	ctfconvert $@

.KEEP_STATE:

clean:
	rm -f save-args.so test
