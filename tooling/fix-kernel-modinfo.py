"""Perform Kbuild's modinfo cleanup without sed's permission-preserving rename."""
import errno
import os
import re
import stat
import sys


def fix_modinfo(path):
    # Match Kbuild's sed expression, including embedded newlines in modinfo.
    # Write the existing inode so no temporary-file permission copy is needed.
    with open(path, 'r+b') as stream:
        data = stream.read()
        cleaned = re.sub(rb'\x00+$', b'\x00', data, flags=re.MULTILINE)
        if cleaned != data:
            stream.seek(0)
            stream.write(cleaned)
            stream.truncate()
        mode = stat.S_IMODE(os.fstat(stream.fileno()).st_mode)
        if mode & 0o111:
            try:
                os.fchmod(stream.fileno(), mode & ~0o111)
            except OSError as error:
                # This is build metadata, never an executable or a guest file.
                # DrvFS may deny chmod or lack support for Unix permission bits.
                if error.errno not in (errno.EPERM, errno.EOPNOTSUPP, errno.ENOSYS):
                    raise


if __name__ == '__main__':
    fix_modinfo(sys.argv[1])
