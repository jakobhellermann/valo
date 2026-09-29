# Debugging a coredump

```sh
cp /tmp/valo.log ./repro/

coredumpctl dump > repro/coredump
gdb -c repro/coredump
```
