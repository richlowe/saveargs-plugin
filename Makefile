SUBDIRS = amd64 aarch64

all := TARGET = save-args.so
test := TARGET = test
clean := TARGET = clean

all test clean: $(SUBDIRS)

$(SUBDIRS): FRC
	make -wC $@ $(TARGET)

FRC:
