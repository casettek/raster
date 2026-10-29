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

# 2. Use Perl to pick random (byte, bit) until bit is 1, then clear it
perl -e '
    my ($file, $file_size) = @ARGV;
    $file_size = int($file_size);
    open(my $fh, "+<:raw", $file) or die "Cannot open $file: $!";
    my $max_attempts = 10000;
    for my $attempt (1 .. $max_attempts) {
        my $byte_offset = int(rand($file_size));
        my $bit_offset = int(rand(8));
        seek($fh, $byte_offset, 0) or die "Cannot seek: $!";
        read($fh, my $byte, 1) or die "Cannot read: $!";
        my $ord = ord($byte);
        if (($ord >> $bit_offset) & 1) {
            my $new_byte = chr($ord & ~(1 << $bit_offset));
            seek($fh, $byte_offset, 0) or die "Cannot seek back: $!";
            print $fh $new_byte;
            close($fh);
            print "Disabled bit at byte $byte_offset, bit $bit_offset.\n";
            exit 0;
        }
    }
    close($fh);
    die "No set bit found after $max_attempts attempts.\n";
' "$FILE" "$FILE_SIZE"

echo "Bit disabled successfully in $FILE."