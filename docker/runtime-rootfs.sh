#!/bin/sh
set -eu

# 静态程序只携带运行所需文件，保留证书、时区和运维命令，不携带包管理器或动态库。
mkdir -p /rootfs/bin /rootfs/etc/ssl/certs /rootfs/usr/share \
    /rootfs/opt/naxus/zcf /rootfs/var/lib/naxus \
    /rootfs/usr/local/logs/naxus /rootfs/var/backups/naxus /rootfs/tmp
cp /bin/busybox.static /rootfs/bin/busybox
for command in sh cat id ls stat date du mkdir chmod chown rm cp mv head tail wc readlink printf echo true false; do
    ln -s busybox "/rootfs/bin/$command"
done
cp /etc/ssl/certs/ca-certificates.crt /rootfs/etc/ssl/certs/
ln -s certs/ca-certificates.crt /rootfs/etc/ssl/cert.pem
cp -a /usr/share/zoneinfo /rootfs/usr/share/
ln -s /usr/share/zoneinfo/Etc/UTC /rootfs/etc/localtime
printf 'root:x:0:0:root:/root:/bin/sh\nnaxus:x:10001:10001:Naxus:/var/lib/naxus:/bin/sh\n' > /rootfs/etc/passwd
printf 'root:x:0:\nnaxus:x:10001:\n' > /rootfs/etc/group
chmod 1777 /rootfs/tmp

# 数据根可能保存初始凭据和发布令牌，启动前限定访问者，日志及备份目录交给运行身份写入。
chown 10001:10001 /rootfs/var/lib/naxus /rootfs/usr/local/logs/naxus /rootfs/var/backups/naxus
chmod 0700 /rootfs/var/lib/naxus
