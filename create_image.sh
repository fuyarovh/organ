IMGSIZE=$((`du -s registers | awk '{print $1}'`/940 + 3072))
cargo build --release
dd if=/dev/zero of=organ.img bs=1M count=$IMGSIZE
sgdisk -o -a 2024 --new=1:+0:+200M organ.img -t=0:ef00 -N=2 -t=2:4f68bce3-e8cd-4db1-96e7-fbcaf984b709
LOOP_DEVICE=`udisksctl loop-setup -f organ.img | awk '{print substr($NF,1, length($NF)-1)}'`
sudo mkfs.fat -F 32 $LOOP_DEVICE"p1"
sudo mkfs.ext4 $LOOP_DEVICE"p2"
sudo mkdir rootfs
sudo mount $LOOP_DEVICE"p2" rootfs
sudo mkdir rootfs/boot
sudo mount $LOOP_DEVICE"p1" rootfs/boot
sudo mkdir -p rootfs/boot/EFI/BOOT
sudo pacstrap -Kc rootfs base linux-rt linux-firmware systemd-ukify intel-ucode pipewire-jack openssh vi polkit cloud-guest-utils parted e2fsprogs dhcpcd rtkit pipewire-alsa usbutils alsa-utils less nano rtkit calf

sudo cp mkinitcpio.conf rootfs/etc/
sudo cp pipewire.conf rootfs/etc/pipewire/
sudo cp first_start.service rootfs/etc/systemd/system/
sudo cp organ.service rootfs/etc/systemd/user/
sudo cp first_start.sh rootfs/usr/bin/
sudo cp 99-hid-permissions.rules rootfs/etc/udev/rules.d/

sudo mount -t proc none rootfs/proc
sudo mount -o bind /dev/ rootfs/dev

sudo chroot rootfs bash -c '
mkinitcpio -P
ukify build --linux=/boot/vmlinuz-linux-rt --initrd=/boot/initramfs-linux-rt.img --microcode=/boot/intel-ucode.img --cmdline="quiet rw" --output=/boot/EFI/BOOT/BOOTx64.EFI
systemctl enable sshd
systemctl enable first_start
systemctl enable dhcpcd
systemctl --global enable organ
useradd -m -G wheel,audio organ
echo "organ:organ" | chpasswd
echo "root:organ" | chpasswd
'

cp ./target/release/organ rootfs/home/organ/organ
cp -r ./registers/ rootfs/home/organ/registers

sudo sync
sudo umount rootfs/boot
sudo sync
sudo fuser -ck rootfs

sudo umount -R rootfs

udisksctl loop-delete -b $LOOP_DEVICE
sudo rmdir rootfs
