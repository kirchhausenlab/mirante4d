#!/bin/busybox sh

set -u

export PATH=/bin
export HOME=/tmp
export TMPDIR=/tmp

mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev

mkdir -p /tmp /mnt/project-a /mnt/project-b

role=""
case_name=""
transition=""
lane=""
edge=""
occurrence=""
for token in $(cat /proc/cmdline); do
  case "$token" in
    m4d.role=*) role=${token#m4d.role=} ;;
    m4d.case=*) case_name=${token#m4d.case=} ;;
    m4d.transition=*) transition=${token#m4d.transition=} ;;
    m4d.lane=*) lane=${token#m4d.lane=} ;;
    m4d.edge=*) edge=${token#m4d.edge=} ;;
    m4d.occurrence=*) occurrence=${token#m4d.occurrence=} ;;
  esac
done

mount -t ext4 -o rw,relatime /dev/vda /mnt/project-a
mount -t ext4 -o rw,relatime /dev/vdb /mnt/project-b

device_a=$(stat -c '%d' /mnt/project-a)
device_b=$(stat -c '%d' /mnt/project-b)
magic_a=$(stat -f -c '%t' /mnt/project-a)
magic_b=$(stat -f -c '%t' /mnt/project-b)
mount_a=$(grep ' /mnt/project-a ' /proc/self/mountinfo | head -n 1)
mount_b=$(grep ' /mnt/project-b ' /proc/self/mountinfo | head -n 1)
vfs_a=$(echo "$mount_a" | awk '{print $6}')
vfs_b=$(echo "$mount_b" | awk '{print $6}')
super_a=$(echo "$mount_a" | awk '{for (i = 1; i <= NF; i++) if ($i == "-") {print $(i + 3); exit}}')
super_b=$(echo "$mount_b" | awk '{for (i = 1; i <= NF; i++) if ($i == "-") {print $(i + 3); exit}}')
echo "mirante4d-project-store-vm-filesystem:project-a|$device_a|$magic_a|$vfs_a|$super_a"
echo "mirante4d-project-store-vm-filesystem:project-b|$device_b|$magic_b|$vfs_b|$super_b"

export MIRANTE4D_PROJECT_STORE_VM_ROLE="$role"
export MIRANTE4D_PROJECT_STORE_VM_CASE="$case_name"
export MIRANTE4D_PROJECT_STORE_VM_TRANSITION="$transition"
export MIRANTE4D_PROJECT_STORE_VM_LANE="$lane"
export MIRANTE4D_PROJECT_STORE_VM_EDGE="$edge"
export MIRANTE4D_PROJECT_STORE_VM_OCCURRENCE="$occurrence"
export MIRANTE4D_PROJECT_STORE_VM_ROOT_A=/mnt/project-a
export MIRANTE4D_PROJECT_STORE_VM_ROOT_B=/mnt/project-b
export MIRANTE4D_PROJECT_STORE_VM_FIXTURE=/fixtures/project-store-v1.tar.gz
export MIRANTE4D_PROJECT_STORE_TEST_REAL_FILESYSTEM_POLICY=1

/mirante4d-project-store-tests \
  actor::tests::durability_tests::project_store_vm_guest_driver \
  --exact --nocapture
status=$?
echo "mirante4d-project-store-vm-driver-exit:$status"
poweroff -f
