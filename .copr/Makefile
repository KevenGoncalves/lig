# Copr's "make srpm" method: an SRPM of the release named in Cargo.toml, from its tag.
version := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
outdir ?= .

srpm:
	dnf -y install git rpm-build
	git config --global --add safe.directory "$(CURDIR)"
	ref=$$(git rev-parse -q --verify "refs/tags/v$(version)" || echo HEAD); \
	git archive --prefix=lig-$(version)/ -o "$(outdir)/lig-$(version).tar.gz" "$$ref"
	sed -e 's/@VERSION@/$(version)/g' -e "s/@DATE@/$$(LC_ALL=C date -u '+%a %b %d %Y')/" packaging/rpm/lig.spec > "$(outdir)/lig.spec"
	rpmbuild -bs --define '_sourcedir $(outdir)' --define '_srcrpmdir $(outdir)' "$(outdir)/lig.spec"
