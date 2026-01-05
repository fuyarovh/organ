#!/bin/bash
PART=`df --output=source / | tail -1`
DISK=/dev/`lsblk -ndo pkname $PART`
parted --fix -s $DISK
growpart $DISK 2
resize2fs $PART
systemctl disable first_start
loginctl enable-linger organ
systemctl reboot
