#!/bin/bash

# Check if a file was provided
if [[ $# -eq 0 ]]; then
    echo "Usage: $0 <filename>"
    exit 1
fi

FILE=$1

# Check if file exists
if [[ ! -f "$FILE" ]]; then
    echo "Error: File '$FILE' not found."
    exit 1
fi

# 1. Get the file size in bytes (portable for macOS and Linux)
if [[ "$(uname)" == "Darwin" ]]; then
    FILE_SIZE=$(stat -f%z "$FILE")
else
    FILE_SIZE=$(stat -c%s "$FILE")
fi

if [ "$FILE_SIZE" -eq 0 ]; then
    echo "Error: File is empty."
    exit 1
fi

# 2. Pick a random byte offset (0 to FILE_SIZE - 1)
BYTE_OFFSET=$(( RANDOM % FILE_SIZE ))

# 3. Pick a random bit position (0 to 7)
BIT_OFFSET=$(( RANDOM % 8 ))

echo "Targeting byte $BYTE_OFFSET, bit $BIT_OFFSET..."

# 4. Use Perl to flip the bit at that specific location
# Open file for read+write, seek to byte, read it, XOR to flip bit, write back
perl -e '
    my ($file, $byte_offset, $bit_offset) = @ARGV;
    open(my $fh, "+<:raw", $file) or die "Cannot open $file: $!";
    seek($fh, $byte_offset, 0) or die "Cannot seek: $!";
    read($fh, my $byte, 1) or die "Cannot read: $!";
    my $new_byte = chr(ord($byte) ^ (1 << $bit_offset));
    seek($fh, $byte_offset, 0) or die "Cannot seek back: $!";
    print $fh $new_byte;
    close($fh);
' "$FILE" "$BYTE_OFFSET" "$BIT_OFFSET"

echo "Bit flipped successfully in $FILE."